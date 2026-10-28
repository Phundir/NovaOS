//! Playing a WAV file through the mixer, for the player and the recorder.
//!
//! ## How far ahead
//!
//! The mixer accepts half a second per stream. Keeping it full would make
//! pause and seek answer half a second late, since what is queued plays
//! first. So the stream is kept a quarter of a second ahead: twelve laps
//! of a 20 ms loop before a gap, and a pause that is heard at once --
//! `pause` throws the queue away and moves the file back by what it held.
//!
//! ## Where the file is
//!
//! In memory, whole. A file on this disk cannot exceed `MAX_FILE_SIZE`,
//! 2 MiB, so reading it once costs less than streaming it block by block
//! would cost in code.

use alloc::vec::Vec;

use crate::audio::resample::Converter;
use crate::audio::wav::{self, Info};
use crate::audio::{self, Handle};

/// Samples kept queued in the mixer: a quarter of a second, stereo.
const AHEAD: usize = audio::SAMPLE_RATE as usize / 4 * audio::CHANNELS;

/// Output frames produced per chunk: 40 ms.
const CHUNK_FRAMES: usize = audio::SAMPLE_RATE as usize / 25;

pub struct Playback {
    bytes: Vec<u8>,
    converter: Converter,
    stream: Option<Handle>,
    playing: bool,
    /// Volume of this stream, in 256ths.
    volume: u32,
    name: &'static str,
}

impl Playback {
    /// Reads a WAV file. `name` labels the mixer stream.
    pub fn load(bytes: Vec<u8>, name: &'static str) -> Result<Playback, &'static str> {
        let info = wav::parse(&bytes)?;
        if info.frames() == 0 {
            return Err("fichier WAV vide");
        }
        Ok(Playback {
            converter: Converter::new(info),
            bytes,
            stream: None,
            playing: false,
            volume: audio::VOLUME_UNITY,
            name,
        })
    }

    pub fn info(&self) -> &Info {
        self.converter.info()
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn duration_ms(&self) -> u64 {
        self.info().duration_ms()
    }

    /// Source frames still queued in the mixer, not yet heard.
    fn queued_frames(&self) -> usize {
        let Some(stream) = self.stream else {
            return 0;
        };
        let samples = audio::pending(stream);
        (samples / audio::CHANNELS) * self.info().rate as usize / audio::SAMPLE_RATE as usize
    }

    /// What is being heard, not what has been handed over.
    pub fn position_ms(&self) -> u64 {
        let frame = self.converter.frame().saturating_sub(self.queued_frames());
        frame as u64 * 1000 / self.info().rate.max(1) as u64
    }

    pub fn play(&mut self) -> Result<(), &'static str> {
        if self.stream.is_none() {
            let stream = audio::open(self.name)?;
            audio::set_volume(stream, self.volume);
            self.stream = Some(stream);
        }
        if self.converter.finished() {
            self.converter.seek(0);
        }
        self.playing = true;
        Ok(())
    }

    /// Stops where the listener is, not where the producer is.
    pub fn pause(&mut self) {
        let heard = self.converter.frame().saturating_sub(self.queued_frames());
        if let Some(stream) = self.stream {
            audio::flush(stream);
        }
        self.converter.seek(heard);
        self.playing = false;
    }

    pub fn stop(&mut self) {
        if let Some(stream) = self.stream.take() {
            audio::close(stream);
        }
        self.converter.seek(0);
        self.playing = false;
    }

    pub fn seek_ms(&mut self, milliseconds: u64) {
        let frame = milliseconds * self.info().rate as u64 / 1000;
        if let Some(stream) = self.stream {
            audio::flush(stream);
        }
        self.converter.seek(frame as usize);
    }

    /// Volume of the stream, in 256ths (the master volume applies on top).
    pub fn set_volume(&mut self, volume: u32) {
        self.volume = volume;
        if let Some(stream) = self.stream {
            audio::set_volume(stream, volume);
        }
    }

    pub fn volume(&self) -> u32 {
        self.volume
    }

    /// Keeps the mixer a quarter of a second ahead. Returns `true` when
    /// the file has just finished playing.
    pub fn feed(&mut self) -> bool {
        let (true, Some(stream)) = (self.playing, self.stream) else {
            return false;
        };
        while audio::pending(stream) < AHEAD {
            let chunk = self.converter.next(&self.bytes, CHUNK_FRAMES);
            if chunk.is_empty() {
                break;
            }
            // Always fits: `AHEAD` plus one chunk is below the half
            // second a stream holds.
            let _ = audio::write(stream, &chunk);
        }
        if self.converter.finished() && audio::pending(stream) == 0 {
            self.stop();
            return true;
        }
        false
    }

    /// Gives the stream back. Called when the application closes.
    pub fn close(&mut self) {
        self.stop();
    }
}
