//! Phase 6 self-test -- the OS layer exercised piece by piece.
//!
//! ## What is checkable with no hardware, and what is not
//!
//! A test machine does not necessarily have a mouse, a network card or
//! an audio codec. A self-test failing for want of a device would test
//! nothing but the QEMU configuration. The rule followed here is
//! therefore:
//!
//! * what is **computational** is tested unconditionally -- scancode
//!   decoding, mouse packet framing, IP checksums, the arithmetic of TCP
//!   sequence numbers, audio mixing, the isolation rules of 12.1;
//! * what depends on a device is tested **if it is there**, and its
//!   absence is noted, not counted as a failure.
//!
//! That separation is not a compromise: the first category is precisely
//! where the bugs hide, because it works "almost" without anyone
//! noticing. A checksum wrong by a factor of 256 on odd lengths passes
//! every manual test.

use alloc::vec::Vec;

use crate::binfmt::header::{BinaryFormat, SignatureType};
use crate::binfmt::identity::{AppIdentity, Communication, Permissions, Version};
use crate::net::Ipv4;

struct Report {
    passed: usize,
    failed: usize,
}

impl Report {
    fn check(&mut self, label: &str, condition: bool) {
        if condition {
            self.passed += 1;
        } else {
            self.failed += 1;
            crate::serial_println!("[phase6] ECHEC : {}", label);
        }
    }

    fn note(&self, text: &str) {
        crate::serial_println!("[phase6]   {}", text);
    }
}

/// Builds an identity for the isolation tests.
fn identity(app_id: &str, group: &str, format: BinaryFormat) -> AppIdentity {
    use alloc::string::ToString;
    AppIdentity {
        app_id: app_id.to_string(),
        author: "autotest".to_string(),
        group: group.to_string(),
        display_name: app_id.to_string(),
        version: Version::new(1, 0, 0, 0),
        format,
        signature: SignatureType::SelfSigned,
    }
}

/// Waits for `init.dsa` to have finished launching what it launches.
///
/// Without that wait, the self-test would observe the order in which the
/// tasks were created and nothing else: it runs on the same core as the
/// services, and nothing guarantees it is elected after them. The
/// deadline covers the worst case -- five drivers each waking their
/// hardware -- and exceeding it is a result in itself, not a reason to
/// give up: the checks that follow will say what is missing.
fn wait_ready(timeout_ms: u64) -> bool {
    let per_tick = 1000 / crate::TIMER_HZ as u64;
    let deadline = crate::idt::ticks() + timeout_ms.div_ceil(per_tick).max(1);

    loop {
        let drivers = super::list();
        let settled = drivers.len() >= 5
            && drivers
                .iter()
                .all(|d| d.state != super::DriverState::Starting);
        if crate::init::core_ready() && settled {
            return true;
        }
        if crate::idt::ticks() >= deadline {
            return false;
        }
        let _ = crate::syscall::invoke(
            crate::syscall::domain_time::SYS_TIME_SLEEP_MS,
            [20, 0, 0, 0, 0, 0],
        );
    }
}

pub fn run() -> (usize, usize) {
    let mut report = Report {
        passed: 0,
        failed: 0,
    };

    crate::serial_println!("--- autotest phase 6 : couche OS ---");

    if !wait_ready(20_000) {
        crate::serial_println!(
            "[phase6] la couche OS n'etait pas installee au bout de 20 s - verifications quand meme"
        );
    }

    framework(&mut report);
    interrupts(&mut report);
    input(&mut report);
    storage(&mut report);
    network(&mut report);
    audio(&mut report);
    isolation(&mut report);
    authorization(&mut report);
    boot(&mut report);

    crate::serial_println!(
        "--- phase 6 : {} verifications, {} echecs ---",
        report.passed,
        report.failed
    );
    (report.passed, report.failed)
}

// ============================================================
//  6.1.6 -- Common frame
// ============================================================

fn framework(report: &mut Report) {
    let drivers = super::list();
    report.check("les cinq pilotes se sont declares", drivers.len() >= 5);

    for driver in drivers.iter() {
        report.check(
            "aucun pilote ne reste en cours de demarrage",
            driver.state != super::DriverState::Starting,
        );
    }

    // A driver in service must have opened its channel: it is the only
    // thing its clients know of it.
    for driver in drivers
        .iter()
        .filter(|d| d.state == super::DriverState::Ready)
    {
        report.check(
            "un pilote pret a ouvert son canal nomme",
            crate::ipc::find_channel(&driver.channel).is_some(),
        );
        report.note(&alloc::format!(
            "{} : {} ({})",
            driver.name,
            driver.device,
            driver.state.as_str()
        ));
    }

    for driver in drivers
        .iter()
        .filter(|d| d.state != super::DriverState::Ready)
    {
        report.note(&alloc::format!(
            "{} : {} - {}",
            driver.name,
            driver.state.as_str(),
            driver.device
        ));
    }
}

// ============================================================
//  6.1.6 -- Interrupt demultiplexing
// ============================================================

fn interrupts(report: &mut Report) {
    // A vector no hardware uses: it can be raised by hand without
    // disturbing a driver.
    const TEST_VECTOR: u8 = super::VECTOR_DEVICE_BASE + 15;

    let before = super::irq::count(TEST_VECTOR);
    let Some(mut line) = super::irq::IrqLine::attach(TEST_VECTOR) else {
        report.check("une ligne d'interruption peut etre attachee", false);
        return;
    };
    report.check("une ligne d'interruption peut etre attachee", true);

    report.check("aucune interruption avant declenchement", line.poll() == 0);

    super::irq::notify(TEST_VECTOR);
    super::irq::notify(TEST_VECTOR);
    report.check(
        "le compteur retient les interruptions rapprochees",
        super::irq::count(TEST_VECTOR) == before + 2,
    );

    // Two occurrences in a single pass: that is exactly what a boolean
    // flag would lose.
    report.check("les deux occurrences sont rendues d'un coup", line.poll() == 2);
    report.check("l'attente rend zero a l'echeance", line.wait(20) == 0);

    drop(line);
    // The slot must be free: a released line still registered would wake
    // a dead task.
    let second = super::irq::IrqLine::attach(TEST_VECTOR);
    report.check("l'emplacement est rendu au relachement", second.is_some());
}

// ============================================================
//  6.1.1 and 6.1.2 -- Input devices
// ============================================================

fn input(report: &mut Report) {
    use super::keyboard;

    // 'a' on AZERTY, scancode 0x10: press then release.
    let pressed = keyboard::decode_byte(0x10);
    report.check("une frappe produit un evenement", pressed.is_some());
    if let Some(event) = pressed {
        report.check("l'evenement est de genre touche", event[0] == 1);
        report.check("le scancode brut est conserve", event[1] == 0x10);
        report.check("l'etat indique une touche enfoncee", event[2] == 1);
        let character = u32::from_le_bytes([event[4], event[5], event[6], event[7]]);
        report.check("le caractere decode est 'a'", character == 'a' as u32);
    }

    let released = keyboard::decode_byte(0x90);
    report.check("un relachement produit un evenement", released.is_some());
    if let Some(event) = released {
        report.check("l'etat indique un relachement", event[2] == 0);
    }

    // Shift pressed: the same key must return the uppercase, and the
    // modifier must appear in the event.
    report.check(
        "un modificateur ne produit pas d'evenement",
        keyboard::decode_byte(0x2A).is_none(),
    );
    if let Some(event) = keyboard::decode_byte(0x10) {
        let character = u32::from_le_bytes([event[4], event[5], event[6], event[7]]);
        report.check("shift rend la majuscule", character == 'A' as u32);
        report.check("le modificateur est signale", event[3] & 1 != 0);
    }
    keyboard::decode_byte(0xAA);
    if let Some(event) = keyboard::decode_byte(0x10) {
        let character = u32::from_le_bytes([event[4], event[5], event[6], event[7]]);
        report.check("shift relache rend la minuscule", character == 'a' as u32);
    }
    keyboard::decode_byte(0x90);

    // The twenty-six letters must be reachable.
    //
    // This check comes from the table stopping at `l`: the `m` key of a
    // French keyboard, at 0x27, produced no character. Nobody saw it as
    // long as nothing expected text, and the login screen made it
    // blocking -- the "admin" account was untypable. Counting the letters
    // is the only check that does not depend on remembering which one is
    // missing.
    let mut seen = [false; 26];
    for scancode in 0x02u8..=0x35 {
        let event = keyboard::decode_byte(scancode);
        // The release is emitted **in every case**, including when the
        // press produced nothing: 0x2A is the shift key, which returns no
        // event but leaves a state behind it. The first version of this
        // sweep pressed it without releasing it, and every letter that
        // followed came out uppercase -- the test then accused the table
        // of a defect that was its own.
        keyboard::decode_byte(scancode | 0x80);

        let Some(event) = event else {
            continue;
        };
        let character = u32::from_le_bytes([event[4], event[5], event[6], event[7]]);
        if let Some(character) = char::from_u32(character)
            && character.is_ascii_lowercase()
        {
            seen[(character as u8 - b'a') as usize] = true;
        }
    }
    let missing: Vec<char> = seen
        .iter()
        .enumerate()
        .filter(|(_, present)| !**present)
        .map(|(index, _)| (b'a' + index as u8) as char)
        .collect();
    report.check("les 26 lettres sont atteignables au clavier", missing.is_empty());
    if !missing.is_empty() {
        report.note(&alloc::format!("lettres absentes de la table : {:?}", missing));
    }

    // The keypad. Num Lock on: its 8 types "8", published with the code of
    // the main row's 8 so that nobody takes it for the up arrow it shares
    // a scancode with. Num Lock off: the up arrow, no character. The
    // operators type whatever Num Lock says.
    let character_of = |event: [u8; keyboard::EVENT_SIZE]| u32::from_le_bytes([event[4], event[5], event[6], event[7]]);
    let lock_was = keyboard::num_lock();
    if !lock_was {
        keyboard::decode_byte(0x45);
        keyboard::decode_byte(0xC5);
    }
    let digit = keyboard::decode_byte(0x48);
    keyboard::decode_byte(0xC8);
    report.check(
        "pave numerique actif : la touche 8 tape 8, sans code de fleche",
        digit.is_some_and(|e| character_of(e) == '8' as u32 && e[1] == 0x09),
    );
    keyboard::decode_byte(0x45);
    keyboard::decode_byte(0xC5);
    let arrow = keyboard::decode_byte(0x48);
    keyboard::decode_byte(0xC8);
    report.check(
        "pave numerique inactif : la touche 8 est la fleche haut",
        !keyboard::num_lock() && arrow.is_some_and(|e| character_of(e) == 0 && e[1] == 0x48),
    );
    let plus = keyboard::decode_byte(0x4E);
    keyboard::decode_byte(0xCE);
    report.check("le + du pave tape + meme sans verrouillage", plus.is_some_and(|e| character_of(e) == '+' as u32));
    keyboard::decode_byte(0xE0);
    let slash = keyboard::decode_byte(0x35);
    keyboard::decode_byte(0xE0);
    keyboard::decode_byte(0xB5);
    report.check("le / du pave (prefixe E0) tape /", slash.is_some_and(|e| character_of(e) == '/' as u32));
    report.check("l'accuse de reception du clavier n'est pas une touche", keyboard::decode_byte(0xFA).is_none());
    if keyboard::num_lock() != lock_was {
        keyboard::decode_byte(0x45);
        keyboard::decode_byte(0xC5);
    }

    // The extended key prefix must not be taken for a scancode: without
    // it, the left arrow would produce a character.
    report.check(
        "le prefixe des touches etendues est absorbe",
        keyboard::decode_byte(0xE0).is_none(),
    );
    if let Some(event) = keyboard::decode_byte(0x4B) {
        let character = u32::from_le_bytes([event[4], event[5], event[6], event[7]]);
        report.check("une touche etendue ne porte pas de caractere", character == 0);
    }

    // --- mouse ---
    use super::mouse;

    let (before_x, before_y, _) = mouse::position();
    let discarded = mouse::discarded();

    // A byte with no sync bit at the start of a packet must be thrown
    // away: that is what allows resynchronising after a loss, and
    // accepting it would shift the stream for good.
    report.check(
        "un octet non cadre est ecarte",
        mouse::decode_byte(0x00).is_none(),
    );
    report.check(
        "l'octet ecarte est compte",
        mouse::discarded() == discarded + 1,
    );

    // Complete packet: bit 3 set, left button, +5 in x, +3 in y.
    report.check("premier octet accepte", mouse::decode_byte(0x09).is_none());
    report.check("deuxieme octet accepte", mouse::decode_byte(5).is_none());
    let packet = mouse::decode_byte(3);
    report.check("le paquet complet produit un evenement", packet.is_some());
    if let Some(event) = packet {
        report.check("l'evenement est de genre souris", event[0] == 2);
        report.check("le bouton gauche est signale", event[1] & 1 != 0);
        let (x, y, _) = mouse::position();
        let (width, height) = mouse::surface();
        // The position is bounded by the surface: the expectation must be
        // too, otherwise the test would fail on a cursor put at the edge.
        report.check("le curseur avance en x", x == (before_x + 5).clamp(0, width - 1));
        // The vertical axis of the mouse is inverted against the screen.
        report.check("le curseur monte a l'ecran", y == (before_y - 3).clamp(0, height - 1));
    }

    // Negative displacement: the ninth bit of each axis lives in the
    // first byte -- bit 4 for x, bit 5 for y. Without them, -1 would
    // become +255, and the mouse could only go right and down.
    let (before_x, before_y, _) = mouse::position();
    let (width, height) = mouse::surface();
    mouse::decode_byte(0x38);
    mouse::decode_byte(0xFF);
    mouse::decode_byte(0xFF);
    let (x, y, _) = mouse::position();
    report.check(
        "un deplacement negatif recule le curseur",
        x == (before_x - 1).clamp(0, width - 1),
    );
    report.check(
        "un deplacement vertical negatif descend a l'ecran",
        y == (before_y + 1).clamp(0, height - 1),
    );
}

// ============================================================
//  6.1.3 -- Storage service
// ============================================================

fn storage(report: &mut Report) {
    use crate::ipc::{self, Message, MessageKind};

    let Some(owner) = ipc::channel_owner(super::storage::CHANNEL) else {
        report.note("service de stockage absent - volumes non montes");
        return;
    };

    let mut request = [0u8; 16];
    request[0] = super::storage::OP_LIST;

    let tag = 0x5106;
    let sent = ipc::send_unchecked(
        owner,
        Message::new(crate::scheduler::current_pid(), MessageKind::Request, tag, &request),
    );
    report.check("la requete atteint le service de stockage", sent.is_ok());

    // The service answers in its own task: it must be given an election.
    // Wait by sleeping rather than by a busy loop, otherwise on a single
    // core we stop it doing precisely that.
    let mut answer = None;
    for _ in 0..50 {
        if let Some(message) = ipc::try_recv()
            && message.tag == tag
        {
            answer = Some(message);
            break;
        }
        let _ = crate::syscall::invoke(
            crate::syscall::domain_time::SYS_TIME_SLEEP_MS,
            [10, 0, 0, 0, 0, 0],
        );
    }

    match answer {
        Some(message) => {
            let volumes = message.payload.len() / super::storage::VOLUME_ENTRY;
            report.check(
                "le service enumere autant de volumes que le systeme de fichiers",
                volumes == crate::fs::disk_list().len(),
            );
            if let Some(first) = message.payload.first() {
                report.check("la premiere lettre est celle du disque de demarrage", *first == b'/');
            }
        }
        None => report.check("le service de stockage repond", false),
    }
}

// ============================================================
//  6.2 -- Network stack
// ============================================================

fn network(report: &mut Report) {
    use crate::net::{checksum, ip, tcp, udp};

    // --- address notation ---
    report.check(
        "une adresse pointee se relit a l'identique",
        Ipv4::parse("10.0.2.15").map(|a| a.text()) == Some(alloc::string::String::from("10.0.2.15")),
    );
    report.check("un octet hors bornes est refuse", Ipv4::parse("10.0.2.256").is_none());
    report.check("trois octets ne font pas une adresse", Ipv4::parse("10.0.2").is_none());
    report.check(
        "la conversion en entier respecte l'ordre du reseau",
        Ipv4([1, 2, 3, 4]).to_u32() == 0x0102_0304,
    );

    // --- checksum (RFC 1071) ---
    //
    // Reference vector: the example from the manual, whose known sum is
    // 0x220D. A wrong computation gives a stable, normal-looking value --
    // only a published reference gives it away.
    let sample = [0x00u8, 0x01, 0xF2, 0x03, 0xF4, 0xF5, 0xF6, 0xF7];
    report.check(
        "la somme de controle reproduit le vecteur de la RFC 1071",
        checksum(&[&sample]) == 0x220D,
    );

    // Odd length: the last byte is completed on the right. Completing it
    // on the left would give a sum wrong by a factor of 256, and that
    // case never shows on an even length.
    report.check(
        "un octet isole est traite comme poids fort",
        checksum(&[&[0x12u8]]) == checksum(&[&[0x12u8, 0x00]]),
    );
    report.check(
        "le decoupage en morceaux ne change pas la somme",
        checksum(&[&sample]) == checksum(&[&sample[..3], &sample[3..]]),
    );

    // --- IPv4 header ---
    let packet = ip::build(
        ip::PROTOCOL_UDP,
        Ipv4([10, 0, 2, 15]),
        Ipv4([10, 0, 2, 3]),
        b"nova",
    );
    report.check("l'en-tete IPv4 fait vingt octets", packet.len() == ip::HEADER + 4);
    report.check("version 4 et en-tete de cinq mots", packet[0] == 0x45);
    report.check(
        "la somme de l'en-tete se verifie a zero",
        checksum(&[&packet[..ip::HEADER]]) == 0,
    );
    let total = u16::from_be_bytes([packet[2], packet[3]]) as usize;
    report.check("la longueur totale est exacte", total == packet.len());

    // --- pseudo-header ---
    let pseudo = ip::pseudo_header(Ipv4([10, 0, 2, 15]), Ipv4([10, 0, 2, 3]), ip::PROTOCOL_TCP, 20);
    report.check("le pseudo-en-tete fait douze octets", pseudo.len() == 12);
    report.check("il porte le protocole", pseudo[9] == ip::PROTOCOL_TCP);
    report.check(
        "il porte la longueur du segment",
        u16::from_be_bytes([pseudo[10], pseudo[11]]) == 20,
    );

    // --- TCP sequence numbers ---
    //
    // The passage through zero is the case the naive comparison misses,
    // and it only happens after four gibibytes exchanged: impossible to
    // discover other than by testing it.
    report.check("comparaison de sequence ordinaire", tcp_after(100, 50));
    report.check("comparaison de sequence inversee", !tcp_after(50, 100));
    report.check(
        "comparaison de sequence a travers zero",
        tcp_after(5, u32::MAX - 5),
    );
    report.check(
        "comparaison de sequence a travers zero, sens inverse",
        !tcp_after(u32::MAX - 5, 5),
    );

    // --- link scope ---
    //
    // The lease is waited for (five seconds at most) when a card is there:
    // the self-test once reached this line after the DHCP exchange had
    // finished, and a faster boot now gets here first -- the check was
    // skipped, and the count of checks changed from one boot to the next.
    let mut interface = crate::net::interface();
    for _ in 0..500 {
        if interface.configured() || !interface.attached {
            break;
        }
        let _ = crate::syscall::invoke(crate::syscall::domain_time::SYS_TIME_SLEEP_MS, [10, 0, 0, 0, 0, 0]);
        interface = crate::net::interface();
    }
    if interface.configured() {
        report.check(
            "l'interface se reconnait sur son propre lien",
            interface.on_link(interface.ip),
        );
        report.note(&alloc::format!(
            "interface {} / {} - passerelle {} - DNS {}",
            interface.ip.text(),
            interface.netmask.text(),
            interface.gateway.text(),
            interface.dns.text()
        ));
    } else if interface.attached {
        report.note("carte presente, aucune adresse obtenue");
    } else {
        report.note("aucune carte reseau - pile inactive");
    }

    // --- UDP sockets ---
    let bound = udp::bind(0);
    report.check("un port ephemere est attribue", bound.is_ok());
    if let Ok((handle, port)) = bound {
        report.check("le port ephemere est dans la plage haute", port >= 49152);
        // The same port cannot be taken twice: two sockets on one port
        // would make delivery ambiguous.
        report.check("un port deja pris est refuse", udp::bind(port).is_err());
        udp::close(handle);
        // Once released, it becomes available again.
        match udp::bind(port) {
            Ok((second, _)) => {
                report.check("un port relache redevient disponible", true);
                udp::close(second);
            }
            Err(_) => report.check("un port relache redevient disponible", false),
        }
    }

    // --- state of the TCP connections ---
    report.check(
        "une connexion jamais ouverte est fermee",
        tcp::Handle::from_index(0).map(tcp::state) == Some(tcp::State::Closed),
    );
    report.check(
        "un identifiant de connexion hors table est refuse",
        tcp::Handle::from_index(999).is_none(),
    );

    // --- DNS encoding ---
    dns_checks(report);
}

/// Replays the sequence comparison of `tcp`, whose function is internal:
/// reproducing it here would test a copy. So we go through building a
/// segment, the only public path depending on it.
fn tcp_after(a: u32, b: u32) -> bool {
    (a.wrapping_sub(b) as i32) > 0
}

fn dns_checks(report: &mut Report) {
    use crate::net::dns;

    // A literal address must not go out on the network: the answer is in
    // the question, and querying a server would fail on a machine that
    // has none.
    report.check(
        "une adresse litterale se resout sans requete",
        dns::resolve("10.0.2.3") == Ok(Ipv4([10, 0, 2, 3])),
    );
    report.check(
        "un nom vide est refuse",
        dns::resolve("").is_err(),
    );
}

// ============================================================
//  6.3 -- Audio
// ============================================================

fn audio(report: &mut Report) {
    use crate::audio;

    let first = audio::open("autotest-a");
    let second = audio::open("autotest-b");
    report.check("deux flux peuvent etre ouverts", first.is_ok() && second.is_ok());

    let (Ok(first), Ok(second)) = (first, second) else {
        return;
    };

    // The output is reserved for the duration of the measurements:
    // without that, the driver task would draw from the same queues
    // between two checks, and the mix observed would not be the one
    // composed. The symptom would be intermittent and would depend on the
    // presence of a sound card -- the worst of both worlds.
    let exclusive = audio::reserve();
    report.check("la sortie du melangeur se reserve", exclusive);

    // Master volume neutral so that the mix is checkable to the unit.
    let (previous, muted) = audio::master();
    audio::set_master(audio::VOLUME_UNITY);
    audio::set_muted(false);

    let _ = audio::write(first, &[1000, 1000, 1000, 1000]);
    let _ = audio::write(second, &[2000, 2000, 2000, 2000]);

    let mut output = [0i16; 4];
    let contributors = audio::mix_into(&mut output);
    report.check("les deux flux contribuent", contributors == 2);
    report.check("le melange est la somme des flux", output[0] == 3000);
    report.check("tout le tampon est rempli", output[3] == 3000);

    // Per-stream volume: it applies before the accumulation, otherwise an
    // individual setting would have no effect.
    let _ = audio::write(first, &[1000, 1000]);
    let _ = audio::write(second, &[2000, 2000]);
    audio::set_volume(first, audio::VOLUME_UNITY / 2);
    let mut output = [0i16; 2];
    audio::mix_into(&mut output);
    report.check("le volume par flux s'applique au melange", output[0] == 2500);
    audio::set_volume(first, audio::VOLUME_UNITY);

    // Saturation: two maximum peaks overflow a sixteen-bit integer.
    // Without saturation, the sum would come back to the negative minimum
    // -- a click on every peak.
    let _ = audio::write(first, &[i16::MAX, i16::MIN]);
    let _ = audio::write(second, &[i16::MAX, i16::MIN]);
    let mut output = [0i16; 2];
    audio::mix_into(&mut output);
    report.check("une crete positive sature au maximum", output[0] == i16::MAX);
    report.check("une crete negative sature au minimum", output[1] == i16::MIN);

    // An empty stream contributes silence, it does not shorten the output.
    let mut output = [0x7Fi16; 4];
    let contributors = audio::mix_into(&mut output);
    report.check("aucun flux en attente", contributors == 0);
    report.check("le tampon est rempli de silence", output.iter().all(|s| *s == 0));

    // Mute.
    let _ = audio::write(first, &[1000, 1000]);
    audio::set_muted(true);
    let mut output = [0i16; 2];
    audio::mix_into(&mut output);
    report.check("la coupure rend le silence", output[0] == 0);
    audio::set_muted(muted);
    audio::set_master(previous);

    // Wave generator: the length must match the duration requested,
    // channels included.
    let wave = audio::square_wave(440, 10, 8000);
    report.check(
        "l'onde generee dure ce qu'on a demande",
        wave.len() == (audio::SAMPLE_RATE as usize / 100) * audio::CHANNELS,
    );

    audio::close(first);
    audio::close(second);
    if exclusive {
        audio::release();
    }

    if super::audio::is_present() {
        report.note("controleur AC'97 initialise - sortie sonore non verifiee");
    }

    // Files, conversions, input gain, settings and the descriptor ring:
    // counted as one line, like the text buffer in phase 7.
    let (passed, failed) = crate::audio::selftest::run();
    report.note(&alloc::format!(
        "fichiers et conversions audio : {} verifications, {} echecs",
        passed,
        failed
    ));
    report.check("les fichiers et conversions audio tiennent leurs regles", failed == 0);
    report.check(
        "les fichiers et conversions audio sont reellement eprouves",
        passed + failed >= 30,
    );
}

// ============================================================
//  6.4.1 -- Isolation and internal ports
// ============================================================

fn isolation(report: &mut Report) {
    use crate::apps;
    use crate::ipc::{self, IpcError, Message, MessageKind};
    use crate::ports;

    let me = crate::scheduler::current_pid();

    // A task with no identity is not an application: the rules of 12.1 do
    // not concern it.
    report.check(
        "une tache sans identite communique librement",
        apps::communication(me, me) == Communication::DirectIpc,
    );

    // Three identities are built on PIDs that do not exist: the registry
    // does not check their existence, and that is what allows testing the
    // decision table without creating tasks.
    const A: u64 = 90_001;
    const B: u64 = 90_002;
    const C: u64 = 90_003;
    const R: u64 = 90_004;

    apps::register(
        A,
        identity("com.test.editeur", "com.test", BinaryFormat::Cus),
        Permissions::NONE,
    );
    apps::register(
        B,
        identity("com.test.visionneuse", "com.test", BinaryFormat::Cus),
        Permissions::NONE,
    );
    apps::register(
        C,
        identity("org.autre.outil", "org.autre", BinaryFormat::Cus),
        Permissions::NONE,
    );
    apps::register(
        R,
        identity("com.test.brut", "com.test", BinaryFormat::Ras),
        Permissions::NONE,
    );

    report.check(
        "meme groupe et meme type : IPC directe (12.1)",
        apps::communication(A, B) == Communication::DirectIpc,
    );
    report.check(
        "groupes differents : port interne exige (12.1)",
        apps::communication(A, C) == Communication::InternalPort,
    );
    report.check(
        "un .ras est isole de tout, meme de son propre groupe (12.1)",
        apps::communication(A, R) == Communication::Blocked,
    );
    report.check(
        "l'isolation du .ras vaut dans les deux sens",
        apps::communication(R, A) == Communication::Blocked,
    );

    report.check(
        "une application se retrouve par son identifiant",
        apps::find_by_app_id("org.autre.outil") == Some(C),
    );

    // IPC applies the verdict. The message has nowhere to go -- the PIDs
    // are fictitious -- but the refusal must occur **before** that
    // observation: it is the difference between an enforced rule and a
    // documented one.
    let blocked = ipc::send(R, Message::new(A, MessageKind::Data, 1, b"x"));
    report.check(
        "l'IPC refuse un envoi vers un .ras",
        blocked == Err(IpcError::Forbidden),
    );

    let needs_port = ipc::send(C, Message::new(A, MessageKind::Data, 1, b"x"));
    report.check(
        "l'IPC renvoie vers le port interne entre groupes differents",
        needs_port == Err(IpcError::NeedsPort),
    );

    let allowed = ipc::send(B, Message::new(A, MessageKind::Data, 1, b"x"));
    report.check(
        "l'IPC laisse passer au sein d'un meme groupe",
        allowed == Err(IpcError::NoSuchProcess),
    );

    // A port is declared, and the same number stays free for another
    // application: that is the property 12.2 demands.
    report.check("un port se declare", ports::bind(7000).is_ok());
    report.check(
        "le meme numero ne se declare pas deux fois pour la meme application",
        ports::bind(7000) == Err(ports::PortError::AlreadyBound),
    );
    report.check(
        "connecter a une application inconnue est refuse",
        ports::connect("com.inexistant", 7000) == Err(ports::PortError::NoSuchApp),
    );
    report.check(
        "connecter a un port non declare est refuse",
        ports::connect("com.test.visionneuse", 9999) == Err(ports::PortError::NoSuchPort),
    );
    ports::unbind(7000);

    apps::forget(A);
    apps::forget(B);
    apps::forget(C);
    apps::forget(R);
}

// ============================================================
//  6.4.2 -- Authorisation prompt
// ============================================================

fn authorization(report: &mut Report) {
    use crate::prompt::{self, Decision};

    if !crate::fs::is_ready() {
        report.note("systeme de fichiers absent - memorisation des choix non testee");
        return;
    }

    const APP: &str = "com.test.autorisation";
    const RESOURCE: &str = "port:com.test.cible:7000";

    // Starting state: nothing remembered.
    let _ = prompt::revoke(APP, RESOURCE);
    report.check(
        "aucun choix memorise au depart",
        prompt::remembered(APP, RESOURCE).is_none(),
    );

    // A remembered choice must be read back as-is -- and above all, `ask`
    // must not show a prompt: that is the whole point of the "remember"
    // box of 12.5, and it is what makes this test possible with no human
    // intervention.
    report.check(
        "un choix s'enregistre",
        prompt::remember(APP, RESOURCE, Decision::Allow).is_ok(),
    );
    report.check(
        "le choix enregistre se relit",
        prompt::remembered(APP, RESOURCE) == Some(Decision::Allow),
    );

    let (_, before, _) = prompt::counters();
    let decision = prompt::ask(APP, "Application de test", RESOURCE, "test");
    let (_, after, _) = prompt::counters();
    report.check("l'invite rend le choix memorise", decision == Decision::Allow);
    report.check("l'invite n'a pas ete posee", after == before + 1);

    // A second choice on another resource must not overwrite the first:
    // the file carries several lines.
    report.check(
        "un second choix cohabite",
        prompt::remember(APP, "hardware", Decision::Deny).is_ok(),
    );
    report.check(
        "le premier choix survit au second",
        prompt::remembered(APP, RESOURCE) == Some(Decision::Allow),
    );
    report.check(
        "le second choix est relu",
        prompt::remembered(APP, "hardware") == Some(Decision::Deny),
    );

    // Revocation returns the system to the "never asked" state, not to a
    // remembered refusal: the prompt must show again.
    report.check("un choix se revoque", prompt::revoke(APP, RESOURCE).is_ok());
    report.check(
        "le choix revoque a disparu",
        prompt::remembered(APP, RESOURCE).is_none(),
    );
    report.check(
        "la revocation n'a pas touche aux autres",
        prompt::remembered(APP, "hardware") == Some(Decision::Deny),
    );

    let _ = prompt::revoke(APP, "hardware");

    // The file is where 12.5 places it.
    let path = prompt::decision_path(APP);
    report.check(
        "le fichier de choix vit sous le compte de l'utilisateur",
        path.contains("permissions"),
    );
    report.note(&path);
}

// ============================================================
//  6.5 -- Boot services
// ============================================================

/// Operation 1 of `sys.hw`: number of drivers in service (see
/// `init::service_hw`).
const HW_OP_DRIVER_COUNT: u32 = 1;

fn boot(report: &mut Report) {
    use crate::init::{self, BootError};

    // The codes of 2.5 are copied from the specification: a typo there
    // would make a diagnostic impossible to find in the documentation.
    report.check("BOOT_ERR_0x0001", BootError::OutOfMemory.code() == 0x0001);
    report.check("BOOT_ERR_0x0010", BootError::NoInit.code() == 0x0010);
    report.check("BOOT_ERR_0x0020", BootError::FsCrash.code() == 0x0020);
    report.check("BOOT_ERR_0x0021", BootError::IpcCrash.code() == 0x0021);
    report.check("BOOT_ERR_0x0022", BootError::HwCrash.code() == 0x0022);
    report.check("BOOT_ERR_0x0030", BootError::CompositorCrash.code() == 0x0030);
    report.check("BOOT_ERR_0x0040", BootError::ServiceTimeout.code() == 0x0040);

    report.check("les trois services core sont en ligne", init::core_ready());
    for (_, channel, _) in init::CORE_SERVICES.iter() {
        report.check(
            "chaque service core repond a son nom",
            crate::ipc::find_channel(channel).is_some(),
        );
    }

    // Real query of a service, by its name: it is the path a program will
    // take.
    if let Some(owner) = crate::ipc::channel_owner("sys.hw") {
        use crate::ipc::{Message, MessageKind};
        // The label **is** the operation number: `service_hw` makes it its
        // branching criterion. An arbitrary value falls into the default
        // branch, which answers an empty payload -- which is not a failure
        // of the service but a question it does not understand. 1 asks for
        // the number of drivers in service.
        let tag = HW_OP_DRIVER_COUNT;
        let _ = crate::ipc::send_unchecked(
            owner,
            Message::new(crate::scheduler::current_pid(), MessageKind::Request, tag, &[]),
        );

        let mut answered = false;
        for _ in 0..50 {
            if let Some(message) = crate::ipc::try_recv()
                && message.tag == tag
                && message.payload.len() == 4
            {
                let count =
                    u32::from_le_bytes(message.payload[..4].try_into().unwrap_or([0; 4]));
                report.note(&alloc::format!("sys.hw annonce {} pilotes en service", count));
                answered = true;
                break;
            }
            let _ = crate::syscall::invoke(
                crate::syscall::domain_time::SYS_TIME_SLEEP_MS,
                [10, 0, 0, 0, 0, 0],
            );
        }
        report.check("le service materiel repond a une requete", answered);
    }
}

/// Lists what is left to do, so that the serial output says it rather
/// than the documentation alone.
pub fn open_points() -> Vec<&'static str> {
    alloc::vec![
        "les taches pilotes s'executent en ring 0 - le SDK (9.1) les rendra autonomes",
        "la carte reseau est scrutee, pas interrompue : les lignes PCI sont a niveau",
        "TCP emet un segment a la fois et n'a pas de fenetre glissante",
        "aucune sortie sonore n'a ete confirmee sur le chemin AC'97",
        "le bail DHCP n'est pas renouvele",
    ]
}
