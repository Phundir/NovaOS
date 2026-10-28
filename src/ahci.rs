//! Minimal AHCI driver -- one port only, one command at a time.
//!
//! AHCI exposes its controller through MMIO: a block of global registers
//! followed by a 128-byte block per port. The dialogue with the disk does
//! not go through those registers but through structures **in RAM** that
//! the controller reads by itself over DMA:
//!
//! ```text
//!   command list        32 headers of 32 bytes, aligned on 1 KiB
//!    \- header[0] ----> command table, aligned on 128 bytes
//!                        |- FIS  Frame Information Structure, the ATA command
//!                        \- PRDT table of data-region descriptors
//!   receive area        256 bytes, where the disk drops its answers
//! ```
//!
//! The kernel fills those structures, sets a bit in `CI` (Command Issue),
//! and the controller does the rest. We wait by polling that same bit.
//!
//! ## What is deliberately absent
//!
//! * **One command in flight only.** AHCI accepts 32 at a time; using
//!   them assumes a request queue and interrupts, hence an asynchronous
//!   layer that will only exist with the user-space drivers (phase 6.1).
//!   Here everything is polled, hence blocking.
//! * **A bounce buffer.** Transfers go through a dedicated page rather
//!   than through the caller's buffer: that avoids having to translate an
//!   arbitrary virtual address into a physical one, and guarantees the
//!   alignment the controller demands.
//! * **No NCQ, no fine error handling.** A command that fails is
//!   reported, not replayed.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

use crate::fs::block::{BLOCK_SIZE, BlockDevice, BlockError, MediumKind, SECTORS_PER_BLOCK};
use crate::frame::{self, Frame};
use crate::memory::{self, phys_to_virt};
use crate::pci;

// ============================================================
//  Registers
// ============================================================

/// Global registers of the controller (Host Bus Adapter).
mod hba {
    pub const CAP: u64 = 0x00;
    pub const GHC: u64 = 0x04;
    pub const IS: u64 = 0x08;
    /// Ports implemented -- one bit per port.
    pub const PI: u64 = 0x0C;
    pub const VS: u64 = 0x10;

    /// GHC.AE -- switches the controller to AHCI mode rather than legacy IDE.
    pub const GHC_AE: u32 = 1 << 31;
    /// CAP.S64A -- the controller accepts 64-bit DMA addresses.
    pub const CAP_S64A: u32 = 1 << 31;
}

/// Registers of a port, at `0x100 + port * 0x80`.
mod port {
    pub const CLB: u64 = 0x00;
    pub const CLBU: u64 = 0x04;
    pub const FB: u64 = 0x08;
    pub const FBU: u64 = 0x0C;
    pub const IS: u64 = 0x10;
    pub const IE: u64 = 0x14;
    pub const CMD: u64 = 0x18;
    pub const TFD: u64 = 0x20;
    pub const SIG: u64 = 0x24;
    pub const SSTS: u64 = 0x28;
    pub const SERR: u64 = 0x30;
    pub const CI: u64 = 0x38;

    /// CMD.ST -- starts processing of the command list.
    pub const CMD_ST: u32 = 1 << 0;
    /// CMD.FRE -- allows reception of FIS.
    pub const CMD_FRE: u32 = 1 << 4;
    /// CMD.FR -- FIS reception is actually running.
    pub const CMD_FR: u32 = 1 << 14;
    /// CMD.CR -- the command list is actually running.
    pub const CMD_CR: u32 = 1 << 15;

    /// TFD.BSY -- the disk is busy.
    pub const TFD_BSY: u32 = 1 << 7;
    /// TFD.DRQ -- the disk is waiting for a data transfer.
    pub const TFD_DRQ: u32 = 1 << 3;
    /// TFD.ERR -- the last command failed.
    pub const TFD_ERR: u32 = 1 << 0;

    /// Signature of an ordinary SATA disk.
    pub const SIG_ATA: u32 = 0x0000_0101;
    /// Signature of an ATAPI device (optical drive).
    pub const SIG_ATAPI: u32 = 0xEB14_0101;
}

// --- ATA commands ---
const ATA_READ_DMA_EXT: u8 = 0x25;
const ATA_WRITE_DMA_EXT: u8 = 0x35;
const ATA_IDENTIFY: u8 = 0xEC;
const ATA_FLUSH_CACHE_EXT: u8 = 0xEA;

/// Number of polling rounds before giving a command up.
const POLL_LIMIT: u32 = 2_000_000;

// ============================================================
//  MMIO access
// ============================================================

fn read_reg(base: u64, offset: u64) -> u32 {
    unsafe { ((base + offset) as *const u32).read_volatile() }
}

fn write_reg(base: u64, offset: u64, value: u32) {
    unsafe { ((base + offset) as *mut u32).write_volatile(value) }
}

// ============================================================
//  DMA structures
// ============================================================

/// Offsets in a command header (32 bytes).
mod header {
    /// bits 0-4: FIS length in 32-bit words - bit 6: write
    pub const FLAGS: usize = 0x00;
    pub const PRDTL: usize = 0x02;
    pub const PRDBC: usize = 0x04;
    pub const CTBA: usize = 0x08;
    pub const CTBAU: usize = 0x0C;
}

/// Offset of the descriptor table inside the command table.
const PRDT_OFFSET: usize = 0x80;
/// Descriptors one command carries: the command table is one page, the
/// PRDT starts at 0x80 and takes 16 bytes an entry -- 248 would fit;
/// 128 (512 KiB, 1024 sectors) keeps a margin and a round number.
const MAX_SCATTER: usize = 128;

/// Writes an integer into a DMA structure, through the physical window.
fn poke32(phys: u64, offset: usize, value: u32) {
    unsafe { ((phys_to_virt(phys) + offset as u64) as *mut u32).write_volatile(value) };
}

fn poke16(phys: u64, offset: usize, value: u16) {
    unsafe { ((phys_to_virt(phys) + offset as u64) as *mut u16).write_volatile(value) };
}

fn poke8(phys: u64, offset: usize, value: u8) {
    unsafe { ((phys_to_virt(phys) + offset as u64) as *mut u8).write_volatile(value) };
}

fn zero_frame(frame: Frame) {
    unsafe { core::ptr::write_bytes(frame.virt_addr() as *mut u8, 0, 4096) };
}

// ============================================================
//  Disk
// ============================================================

/// A SATA disk behind an AHCI port.
pub struct AhciDisk {
    /// MMIO base of the controller, in the physical window.
    abar: u64,
    /// MMIO base of the port.
    port_base: u64,
    port_index: u32,
    /// Page carrying the command list and the receive area.
    control: Frame,
    /// Page carrying the command table of the single slot used.
    table: Frame,
    /// Bounce page for the data.
    bounce: Frame,
    sectors: u64,
    model: String,
    supports_64bit: bool,
}

impl AhciDisk {
    fn port_reg(&self, offset: u64) -> u32 {
        read_reg(self.port_base, offset)
    }

    fn set_port_reg(&self, offset: u64, value: u32) {
        write_reg(self.port_base, offset, value);
    }

    /// Stops the port, a precondition to reprogramming CLB and FB.
    ///
    /// The order is not negotiable: clear ST then wait for CR to fall,
    /// clear FRE then wait for FR. Reprogramming the addresses while the
    /// controller reads them produces DMA to anywhere at all.
    fn stop(&self) -> Result<(), BlockError> {
        let mut cmd = self.port_reg(port::CMD);
        cmd &= !port::CMD_ST;
        self.set_port_reg(port::CMD, cmd);

        let mut spins = 0;
        while self.port_reg(port::CMD) & port::CMD_CR != 0 {
            spins += 1;
            if spins > POLL_LIMIT {
                return Err(BlockError::Timeout);
            }
            core::hint::spin_loop();
        }

        let mut cmd = self.port_reg(port::CMD);
        cmd &= !port::CMD_FRE;
        self.set_port_reg(port::CMD, cmd);

        let mut spins = 0;
        while self.port_reg(port::CMD) & port::CMD_FR != 0 {
            spins += 1;
            if spins > POLL_LIMIT {
                return Err(BlockError::Timeout);
            }
            core::hint::spin_loop();
        }

        Ok(())
    }

    fn start(&self) {
        // FRE before ST: the controller must be able to drop its answers
        // before it is given work.
        let cmd = self.port_reg(port::CMD) | port::CMD_FRE;
        self.set_port_reg(port::CMD, cmd);
        let cmd = self.port_reg(port::CMD) | port::CMD_ST;
        self.set_port_reg(port::CMD, cmd);
    }

    /// Waits until the disk is neither busy nor waiting for data.
    fn wait_ready(&self) -> Result<(), BlockError> {
        let mut spins = 0;
        loop {
            let tfd = self.port_reg(port::TFD);
            if tfd & (port::TFD_BSY | port::TFD_DRQ) == 0 {
                return Ok(());
            }
            spins += 1;
            if spins > POLL_LIMIT {
                return Err(BlockError::Timeout);
            }
            core::hint::spin_loop();
        }
    }

    /// Prepares and launches an ATA command on slot 0.
    ///
    /// `sectors` is 0 for a command with no data transfer.
    fn issue(
        &self,
        command: u8,
        lba: u64,
        sectors: u16,
        write: bool,
        frames: &[u64],
    ) -> Result<(), BlockError> {
        self.wait_ready()?;

        // The errors of the previous command must be cleared, otherwise
        // the controller refuses to start again.
        self.set_port_reg(port::SERR, self.port_reg(port::SERR));
        self.set_port_reg(port::IS, self.port_reg(port::IS));

        let table_phys = self.table.start_addr();
        let control_phys = self.control.start_addr();
        let bounce_phys = self.bounce.start_addr();

        zero_frame(self.table);

        // --- FIS Register Host-to-Device, 20 bytes ---
        poke8(table_phys, 0, 0x27); // type
        poke8(table_phys, 1, 1 << 7); // bit C: this is a command
        poke8(table_phys, 2, command);
        poke8(table_phys, 3, 0); // features

        poke8(table_phys, 4, lba as u8);
        poke8(table_phys, 5, (lba >> 8) as u8);
        poke8(table_phys, 6, (lba >> 16) as u8);
        // Bit 6 of the device register: LBA addressing rather than CHS.
        poke8(table_phys, 7, 1 << 6);
        poke8(table_phys, 8, (lba >> 24) as u8);
        poke8(table_phys, 9, (lba >> 32) as u8);
        poke8(table_phys, 10, (lba >> 40) as u8);
        poke8(table_phys, 11, 0);

        poke8(table_phys, 12, sectors as u8);
        poke8(table_phys, 13, (sectors >> 8) as u8);

        // --- data region descriptor ---
        let prdtl = if !frames.is_empty() {
            // Scatter: one descriptor per destination frame, 4 KiB each.
            for (i, phys) in frames.iter().enumerate() {
                let at = PRDT_OFFSET + i * 16;
                poke32(table_phys, at, *phys as u32);
                poke32(table_phys, at + 4, (*phys >> 32) as u32);
                poke32(table_phys, at + 8, 0);
                poke32(table_phys, at + 12, BLOCK_SIZE as u32 - 1);
            }
            frames.len() as u16
        } else if sectors == 0 && command != ATA_IDENTIFY {
            0u16
        } else {
            let bytes = if command == ATA_IDENTIFY {
                512u32
            } else {
                sectors as u32 * 512
            };
            poke32(table_phys, PRDT_OFFSET, bounce_phys as u32);
            poke32(table_phys, PRDT_OFFSET + 4, (bounce_phys >> 32) as u32);
            poke32(table_phys, PRDT_OFFSET + 8, 0);
            // The count is expressed "minus one", and bit 31 would ask
            // for a completion interrupt -- pointless since we poll.
            poke32(table_phys, PRDT_OFFSET + 12, bytes - 1);
            1u16
        };

        // --- command header, slot 0 ---
        // 5 words of 32 bits for a FIS Register H2D, plus the write bit.
        let mut flags = 5u16;
        if write {
            flags |= 1 << 6;
        }
        poke16(control_phys, header::FLAGS, flags);
        poke16(control_phys, header::PRDTL, prdtl);
        poke32(control_phys, header::PRDBC, 0);
        poke32(control_phys, header::CTBA, table_phys as u32);
        poke32(control_phys, header::CTBAU, (table_phys >> 32) as u32);

        // --- launch ---
        self.set_port_reg(port::CI, 1);

        let mut spins = 0;
        loop {
            if self.port_reg(port::CI) & 1 == 0 {
                break;
            }
            if self.port_reg(port::TFD) & port::TFD_ERR != 0 {
                self.abort(command, lba, "erreur du disque");
                return Err(BlockError::DeviceError);
            }
            spins += 1;
            if spins > POLL_LIMIT {
                self.abort(command, lba, "delai depasse");
                return Err(BlockError::Timeout);
            }
            core::hint::spin_loop();
        }

        if self.port_reg(port::TFD) & port::TFD_ERR != 0 {
            return Err(BlockError::DeviceError);
        }

        Ok(())
    }

    /// Takes back a command the controller has not finished.
    ///
    /// Returning its error is not enough: the command is still in the
    /// controller's hands, and its DMA may land at any moment in the
    /// frames it was given. The caller, told the read failed, gives those
    /// frames back to the allocator -- a page cache filling a library
    /// hands them to whoever asks next -- and the late transfer then
    /// writes a disk block into somebody else's memory. Clearing ST and
    /// waiting for CR to fall is what AHCI defines as stopping the
    /// command list (1.3.1, 10.1.2): once CR is clear, no DMA of the port
    /// is in flight. The port is then started again for the next command.
    fn abort(&self, command: u8, lba: u64, why: &str) {
        crate::serial_println!(
            "[ahci] port {} : commande {:#04x} (LBA {}) abandonnee - {} ; port arrete puis relance",
            self.port_index, command, lba, why
        );
        if self.stop().is_err() {
            crate::serial_println!("[ahci] port {} : le port ne s'arrete pas", self.port_index);
        }
        self.set_port_reg(port::SERR, self.port_reg(port::SERR));
        self.set_port_reg(port::IS, self.port_reg(port::IS));
        self.start();
    }

    /// Queries the disk: sector count and model.
    fn identify(&mut self) -> Result<(), BlockError> {
        zero_frame(self.bounce);
        self.issue(ATA_IDENTIFY, 0, 0, false, &[])?;

        let data = self.bounce.virt_addr() as *const u16;

        // Words 100 to 103: number of sectors addressable in LBA48.
        let mut sectors: u64 = 0;
        for i in 0..4 {
            let word = unsafe { data.add(100 + i).read_volatile() } as u64;
            sectors |= word << (16 * i);
        }
        if sectors == 0 {
            // Fall back on LBA28, words 60 and 61.
            let low = unsafe { data.add(60).read_volatile() } as u64;
            let high = unsafe { data.add(61).read_volatile() } as u64;
            sectors = low | (high << 16);
        }
        self.sectors = sectors;

        // Words 27 to 46: model, in ASCII, bytes swapped in pairs.
        let mut model = alloc::vec![0u8; 40];
        for i in 0..20 {
            let word = unsafe { data.add(27 + i).read_volatile() };
            model[i * 2] = (word >> 8) as u8;
            model[i * 2 + 1] = word as u8;
        }
        while model.last() == Some(&b' ') || model.last() == Some(&0) {
            model.pop();
        }
        // The log accepts printable ASCII only.
        for byte in model.iter_mut() {
            if !(0x20..=0x7E).contains(byte) {
                *byte = b'?';
            }
        }
        self.model = String::from_utf8(model).unwrap_or_else(|_| String::from("disque SATA"));

        Ok(())
    }
}

impl BlockDevice for AhciDisk {
    fn name(&self) -> String {
        self.model.clone()
    }

    fn kind(&self) -> MediumKind {
        MediumKind::Sata
    }

    fn block_count(&self) -> u64 {
        self.sectors / SECTORS_PER_BLOCK as u64
    }

    fn read_block(&mut self, index: u64, buffer: &mut [u8]) -> Result<(), BlockError> {
        if buffer.len() != BLOCK_SIZE {
            return Err(BlockError::BadBufferSize);
        }
        if index >= self.block_count() {
            return Err(BlockError::OutOfRange);
        }

        let lba = index * SECTORS_PER_BLOCK as u64;
        self.issue(ATA_READ_DMA_EXT, lba, SECTORS_PER_BLOCK as u16, false, &[])?;

        let source = self.bounce.virt_addr() as *const u8;
        unsafe { core::ptr::copy_nonoverlapping(source, buffer.as_mut_ptr(), BLOCK_SIZE) };
        Ok(())
    }

    fn write_block(&mut self, index: u64, buffer: &[u8]) -> Result<(), BlockError> {
        if buffer.len() != BLOCK_SIZE {
            return Err(BlockError::BadBufferSize);
        }
        if index >= self.block_count() {
            return Err(BlockError::OutOfRange);
        }

        let destination = self.bounce.virt_addr() as *mut u8;
        unsafe { core::ptr::copy_nonoverlapping(buffer.as_ptr(), destination, BLOCK_SIZE) };

        let lba = index * SECTORS_PER_BLOCK as u64;
        self.issue(ATA_WRITE_DMA_EXT, lba, SECTORS_PER_BLOCK as u16, true, &[])
    }

    fn flush(&mut self) -> Result<(), BlockError> {
        self.issue(ATA_FLUSH_CACHE_EXT, 0, 0, false, &[])
    }

    /// One READ DMA EXT per `MAX_SCATTER` frames, the controller writing
    /// straight into them: no bounce page, no copy. A page cache filling
    /// 128 KiB of a library costs one command instead of thirty-two.
    fn read_to_frames(&mut self, sector: u64, frames: &[u64]) -> Result<(), BlockError> {
        let per_frame = SECTORS_PER_BLOCK as u64;
        if sector + frames.len() as u64 * per_frame > self.sectors {
            return Err(BlockError::OutOfRange);
        }
        // A controller without 64-bit addressing cannot reach a frame
        // above 4 GiB: through the bounce page, one block at a time.
        if !self.supports_64bit && frames.iter().any(|phys| *phys >> 32 != 0) {
            let mut block = alloc::vec![0u8; BLOCK_SIZE];
            for (i, phys) in frames.iter().enumerate() {
                let lba = sector + i as u64 * per_frame;
                self.issue(ATA_READ_DMA_EXT, lba, SECTORS_PER_BLOCK as u16, false, &[])?;
                let source = self.bounce.virt_addr() as *const u8;
                unsafe { core::ptr::copy_nonoverlapping(source, block.as_mut_ptr(), BLOCK_SIZE) };
                let destination = crate::memory::phys_to_virt(*phys) as *mut u8;
                unsafe { core::ptr::copy_nonoverlapping(block.as_ptr(), destination, BLOCK_SIZE) };
            }
            return Ok(());
        }
        for (i, chunk) in frames.chunks(MAX_SCATTER).enumerate() {
            let lba = sector + (i * MAX_SCATTER) as u64 * per_frame;
            self.issue(ATA_READ_DMA_EXT, lba, (chunk.len() as u64 * per_frame) as u16, false, chunk)?;
        }
        Ok(())
    }
}

// ============================================================
//  Detection
// ============================================================

/// Looks for an AHCI controller and initialises each of its active ports.
///
/// # Safety
/// Reprograms a PCI device and grants it bus mastering: it will be able
/// to write to memory at the addresses it has been given.
pub unsafe fn detect() -> Vec<Box<dyn BlockDevice>> {
    let mut disks: Vec<Box<dyn BlockDevice>> = Vec::new();

    let Some(controller) = pci::find(0x01, 0x06) else {
        crate::serial_println!("[ahci] aucun controleur AHCI sur le bus PCI");
        return disks;
    };

    // Without MEMORY_SPACE the BAR does not answer; without BUS_MASTER
    // the controller cannot read the structures prepared for it.
    unsafe { controller.enable(pci::command::MEMORY_SPACE | pci::command::BUS_MASTER) };

    let abar_phys = controller.bar(5);
    if abar_phys == 0 {
        crate::serial_println!("[ahci] BAR5 nul - controleur inutilisable");
        return disks;
    }

    // MMIO must never be cached: a register write left in the cache would
    // never reach the controller.
    unsafe { memory::set_mmio_uncached(abar_phys) };
    let abar = phys_to_virt(abar_phys);

    // Switch to AHCI mode: without AE the controller emulates a legacy IDE.
    let ghc = read_reg(abar, hba::GHC);
    write_reg(abar, hba::GHC, ghc | hba::GHC_AE);

    // The pending interrupts of the controller are acknowledged: the
    // driver polls, nobody will come and consume them.
    let pending = read_reg(abar, hba::IS);
    write_reg(abar, hba::IS, pending);

    let cap = read_reg(abar, hba::CAP);
    let version = read_reg(abar, hba::VS);
    let implemented = read_reg(abar, hba::PI);
    let supports_64bit = cap & hba::CAP_S64A != 0;

    crate::serial_println!(
        "[ahci] controleur {:04x}:{:04x} - ABAR 0x{:x}, version {}.{}, {} ports, DMA {} bits",
        controller.vendor_id,
        controller.device_id,
        abar_phys,
        version >> 16,
        (version >> 8) & 0xFF,
        (cap & 0x1F) + 1,
        if supports_64bit { 64 } else { 32 }
    );

    for index in 0..32u32 {
        if implemented & (1 << index) == 0 {
            continue;
        }
        let port_base = abar + 0x100 + index as u64 * 0x80;

        // SSTS.DET must be 3 (link established) and IPM 1 (interface active).
        let ssts = read_reg(port_base, port::SSTS);
        if ssts & 0x0F != 3 || (ssts >> 8) & 0x0F != 1 {
            continue;
        }

        let signature = read_reg(port_base, port::SIG);
        if signature != port::SIG_ATA {
            crate::serial_println!(
                "[ahci]   port {} : signature 0x{:08x} ({}) - ignore",
                index,
                signature,
                if signature == port::SIG_ATAPI {
                    "ATAPI"
                } else {
                    "inconnue"
                }
            );
            continue;
        }

        match unsafe { init_port(abar, port_base, index, supports_64bit) } {
            Some(disk) => {
                crate::serial_println!(
                    "[ahci]   port {} : \"{}\" - {} secteurs ({} Mio)",
                    index,
                    disk.model,
                    disk.sectors,
                    disk.sectors * 512 / (1024 * 1024)
                );
                disks.push(Box::new(disk));
            }
            None => {
                crate::serial_println!("[ahci]   port {} : initialisation echouee", index);
            }
        }
    }

    disks
}

/// Prepares the DMA structures of a port and queries the disk.
unsafe fn init_port(
    abar: u64,
    port_base: u64,
    index: u32,
    supports_64bit: bool,
) -> Option<AhciDisk> {
    // Three pages: control structures, command table, bounce. They must
    // be physically contiguous in themselves, which a 4 KiB frame
    // guarantees, and aligned -- 4 KiB covers the requirements of 1 KiB
    // (command list) and 128 bytes (table).
    let control = frame::allocate_zeroed_frame()?;
    let table = frame::allocate_zeroed_frame()?;
    let bounce = frame::allocate_zeroed_frame()?;

    let mut disk = AhciDisk {
        abar,
        port_base,
        port_index: index,
        control,
        table,
        bounce,
        sectors: 0,
        model: String::from("disque SATA"),
        supports_64bit,
    };

    if disk.stop().is_err() {
        return None;
    }

    // Command list at the start of the page, receive area at 1 KiB: the
    // two alignments demanded by the specification are met, and the list
    // occupies exactly 32 x 32 = 1024 bytes.
    let control_phys = control.start_addr();
    let fis_phys = control_phys + 1024;

    write_reg(port_base, port::CLB, control_phys as u32);
    write_reg(port_base, port::CLBU, (control_phys >> 32) as u32);
    write_reg(port_base, port::FB, fis_phys as u32);
    write_reg(port_base, port::FBU, (fis_phys >> 32) as u32);

    // Every interrupt masked: the driver polls.
    write_reg(port_base, port::IE, 0);
    write_reg(port_base, port::SERR, read_reg(port_base, port::SERR));
    write_reg(port_base, port::IS, read_reg(port_base, port::IS));

    disk.start();

    if disk.identify().is_err() || disk.sectors == 0 {
        return None;
    }

    Some(disk)
}

impl AhciDisk {
    /// Diagnostics: raw state of the port.
    pub fn dump_state(&self) {
        crate::serial_println!(
            "[ahci] port {} - CMD 0x{:08x} TFD 0x{:08x} SSTS 0x{:08x} SERR 0x{:08x}",
            self.port_index,
            self.port_reg(port::CMD),
            self.port_reg(port::TFD),
            self.port_reg(port::SSTS),
            self.port_reg(port::SERR)
        );
        crate::serial_println!(
            "[ahci] ABAR 0x{:x} - adressage {} bits",
            self.abar,
            if self.supports_64bit { 64 } else { 32 }
        );
    }
}
