//! Self-test of the audio files and conversions.
//!
//! Everything here is arithmetic that can be checked with no loudspeaker:
//! the WAV layouts, the float conversion done with integers, the rate and
//! channel conversions, the input gain, the settings file, the descriptor
//! ring of the driver. What only an ear can check -- that the result
//! *sounds* right -- is named in the driver and not claimed here.

use alloc::vec::Vec;

use super::capture::apply_gain;
use super::resample::{Converter, Decimator};
use super::settings::{Settings, Source};
use super::wav::{self, Encoding};

struct Count {
    passed: usize,
    failed: usize,
}

impl Count {
    fn check(&mut self, label: &str, condition: bool) {
        if condition {
            self.passed += 1;
        } else {
            self.failed += 1;
            crate::serial_println!("[audio] ECHEC : {}", label);
        }
    }
}

pub fn run() -> (usize, usize) {
    let mut count = Count {
        passed: 0,
        failed: 0,
    };
    wav_files(&mut count);
    floats(&mut count);
    conversions(&mut count);
    input(&mut count);
    settings(&mut count);
    synthesis(&mut count);
    ring(&mut count);
    (count.passed, count.failed)
}

/// A WAV file with a `fmt ` chunk of the given tag and bits, and `data`.
fn build(tag: u16, channels: u16, rate: u32, bits: u16, data: &[u8], extensible: bool) -> Vec<u8> {
    let mut fmt = Vec::new();
    fmt.extend_from_slice(&(if extensible { 0xFFFEu16 } else { tag }).to_le_bytes());
    fmt.extend_from_slice(&channels.to_le_bytes());
    fmt.extend_from_slice(&rate.to_le_bytes());
    let block = channels * bits / 8;
    fmt.extend_from_slice(&(rate * block as u32).to_le_bytes());
    fmt.extend_from_slice(&block.to_le_bytes());
    fmt.extend_from_slice(&bits.to_le_bytes());
    if extensible {
        fmt.extend_from_slice(&22u16.to_le_bytes());
        fmt.extend_from_slice(&bits.to_le_bytes());
        fmt.extend_from_slice(&0u32.to_le_bytes());
        fmt.extend_from_slice(&tag.to_le_bytes());
        fmt.extend_from_slice(&[0u8; 14]);
    }
    let mut file = Vec::new();
    file.extend_from_slice(b"RIFF");
    file.extend_from_slice(&0u32.to_le_bytes());
    file.extend_from_slice(b"WAVE");
    // An unknown chunk first: it must be skipped, odd length and padding
    // included.
    file.extend_from_slice(b"LIST");
    file.extend_from_slice(&3u32.to_le_bytes());
    file.extend_from_slice(&[1, 2, 3, 0]);
    file.extend_from_slice(b"fmt ");
    file.extend_from_slice(&(fmt.len() as u32).to_le_bytes());
    file.extend_from_slice(&fmt);
    file.extend_from_slice(b"data");
    file.extend_from_slice(&(data.len() as u32).to_le_bytes());
    file.extend_from_slice(data);
    file
}

fn wav_files(count: &mut Count) {
    let samples = [0i16, 1000, -1000, i16::MAX, i16::MIN, 7];
    let bytes = wav::encode(48_000, 2, &samples);
    let info = wav::parse(&bytes);
    count.check("un WAV ecrit se relit", info.is_ok());
    if let Ok(info) = info {
        count.check(
            "le format relu est celui ecrit",
            info.rate == 48_000 && info.channels == 2 && info.encoding == Encoding::Integer(16),
        );
        count.check("trois trames stereo", info.frames() == 3);
        let back: Vec<i16> = (0..3)
            .flat_map(|frame| [info.sample(&bytes, frame, 0), info.sample(&bytes, frame, 1)])
            .collect();
        count.check("les echantillons se relisent a l'identique", back == samples);
    }

    let eight = build(1, 1, 8000, 8, &[128, 255, 0], false);
    let info = wav::parse(&eight);
    count.check(
        "8 bits : non signe, centre sur 128, bloc inconnu saute",
        info.is_ok_and(|info| {
            info.sample(&eight, 0, 0) == 0
                && info.sample(&eight, 1, 0) == 127 << 8
                && info.sample(&eight, 2, 0) == -128 << 8
        }),
    );

    let twenty_four = build(1, 1, 44_100, 24, &[0x00, 0x34, 0x12, 0xFF, 0xFF, 0xFF], false);
    let info = wav::parse(&twenty_four);
    count.check(
        "24 bits : les deux octets forts sont gardes",
        info.is_ok_and(|info| {
            info.sample(&twenty_four, 0, 0) == 0x1234 && info.sample(&twenty_four, 1, 0) == -1
        }),
    );

    let extensible = build(1, 2, 44_100, 16, &[0x10, 0x00, 0x20, 0x00], true);
    count.check(
        "le format etendu suit le GUID du sous-format",
        wav::parse(&extensible).is_ok_and(|info| info.encoding == Encoding::Integer(16)),
    );

    let adpcm = build(2, 1, 8000, 4, &[0; 4], false);
    count.check("l'ADPCM est refuse par son nom", wav::parse(&adpcm).is_err());
    count.check("un fichier qui n'est pas un WAV est refuse", wav::parse(b"ID3\x03 mp3").is_err());
    count.check(
        "un bloc data plus long que le fichier est ramene a ce qui existe",
        wav::parse(&bytes[..bytes.len() - 4]).is_ok_and(|info| info.data_len == 8),
    );
}

fn floats(count: &mut Count) {
    count.check("1.0 donne le maximum", wav::float_to_i16(0x3F80_0000) == 32767);
    count.check("-0.5 donne la moitie negative", wav::float_to_i16(0xBF00_0000) == -16383);
    count.check("0.0 donne zero", wav::float_to_i16(0) == 0);
    count.check("au-dela de 1.0 sature", wav::float_to_i16(0x4000_0000) == 32767);
    count.check("NaN donne le silence", wav::float_to_i16(0x7FC0_0000) == 0);
}

fn conversions(count: &mut Count) {
    // 24 kHz mono at a constant value: twice the frames at 48 kHz, stereo,
    // and the constant kept -- interpolating between equal samples must
    // not invent anything.
    let mono: Vec<i16> = alloc::vec![5000; 240];
    let bytes = wav::encode(24_000, 1, &mono);
    if let Ok(info) = wav::parse(&bytes) {
        let mut converter = Converter::new(info);
        let out = converter.next(&bytes, 10_000);
        count.check("24 kHz vers 48 kHz double les trames", out.len() == 480 * 2);
        count.check(
            "une constante reste constante, sur les deux voies",
            out.iter().all(|sample| *sample == 5000),
        );
        count.check("la fin du fichier est atteinte", converter.finished());

        converter.seek(120);
        let rest = converter.next(&bytes, 10_000);
        count.check("un deplacement au milieu joue la moitie restante", rest.len() == 240 * 2);
    }

    // A ramp at 48 kHz: the identity, sample for sample.
    let ramp: Vec<i16> = (0..100).map(|index| index * 10).collect();
    let bytes = wav::encode(48_000, 1, &ramp);
    if let Ok(info) = wav::parse(&bytes) {
        let out = Converter::new(info).next(&bytes, 1000);
        count.check(
            "48 kHz reste echantillon pour echantillon",
            out.chunks(2).map(|frame| frame[0]).eq(ramp.iter().copied()),
        );
    }

    // Halfway between two samples: the mean.
    let pair = [0i16, 1000];
    let bytes = wav::encode(24_000, 1, &pair);
    if let Ok(info) = wav::parse(&bytes) {
        let out = Converter::new(info).next(&bytes, 2);
        count.check("l'interpolation tombe au milieu", out.get(2) == Some(&500));
    }

    let mut decimator = Decimator::new();
    let mut out = Vec::new();
    decimator.push(&[300, 100, 300, 100, 300, 100, 600, 0, 600, 0, 600, 0], &mut out);
    count.check("48 kHz stereo vers 16 kHz mono : trois trames pour une", out == [200, 300]);
}

fn input(count: &mut Count) {
    let mut block = [1000i16, -1000, 20_000, -20_000];
    let (peak, clipped) = apply_gain(&mut block, 2 * super::VOLUME_UNITY, false);
    count.check(
        "le gain double et sature",
        block == [2000, -2000, i16::MAX, i16::MIN] && clipped == 2 && peak == 32767,
    );
    let mut block = [1000i16, -1000];
    let (peak, _) = apply_gain(&mut block, super::VOLUME_UNITY, true);
    count.check("l'entree coupee rend le silence", block == [0, 0] && peak == 0);
}

fn settings(count: &mut Count) {
    let custom = Settings {
        output_volume: 35,
        output_muted: true,
        input_source: Source::Line,
        input_gain: 250,
        input_muted: false,
    };
    count.check(
        "les reglages survivent a leur fichier",
        Settings::parse(&custom.serialize()) == custom,
    );
    let broken = Settings::parse("sortie_volume = 900\nentree_source = casque\nfoo\nentree_gain=abc\n");
    count.check("une valeur hors bornes garde le defaut", broken == Settings::DEFAULT);
    count.check(
        "100 % est l'unite du melangeur",
        super::settings::percent_to_volume(100) == super::VOLUME_UNITY,
    );
}

fn synthesis(count: &mut Count) {
    use super::synth::sine;
    let quarter = 1 << 14;
    count.check("le sinus vaut 1 au quart de periode", sine(quarter, 10_000) == 10_000);
    count.check("le sinus vaut 0 a la demi-periode", sine(2 * quarter, 10_000) == 0);
    count.check("le sinus vaut -1 aux trois quarts", sine(3 * quarter, 10_000) == -10_000);
    // sin(30 degrees) = 0.5: Bhaskara gives it exactly, and the integer
    // phase of 30 degrees is truncated by a third of a unit -- one step.
    count.check(
        "sin(30) vaut 0.5 a un pas pres",
        (4999..=5001).contains(&sine((1 << 15) / 6, 10_000)),
    );
}

fn ring(count: &mut Count) {
    use crate::drivers::audio::{BUFFERS, RING, ahead};
    count.check("l'anneau compte en avant", ahead(3, 7) == 4);
    count.check("l'anneau fait le tour a 32", ahead(30, 2) == 4);
    count.check("un anneau vide est a zero", ahead(9, 9) == 0);
    count.check(
        "quatre tampons, huit tours d'anneau",
        RING % BUFFERS == 0 && RING / BUFFERS == 8,
    );
}
