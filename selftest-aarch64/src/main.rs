//! AArch64 hardware self-test.
//!
//! Boots as `kernel8.img`, runs a fixed list of checks top to bottom, draws the
//! results and a QR encoding them, and then alternates between the results and a
//! cache write-back demonstration. It never reboots and it never writes to the
//! SD card.
//!
//! The order matters. The mailbox work runs with the MMU off, so the message
//! buffer the firmware reads through the bus is plain physical memory. The
//! framebuffer is acquired in the same state and immediately filled grey, which
//! makes a live scanout obvious before any other result exists. The MMU and
//! caches are then enabled deliberately, so the coherence experiment is a step
//! this program takes rather than a state it inherited.
#![no_std]
#![no_main]

use core::{fmt::Write,
           panic::PanicInfo,
           ptr::{read_volatile, write_volatile}};

use mac_sdhci::Disk;
use mac_videocore_fb::{Error, Framebuffer, Mode, PixelOrder, acquire};

mod cache;
mod dma;
mod font;
mod mailbox;
mod mbr;
mod mmu;
mod platform;
mod qr;
mod screen;
mod spi0;
mod uart;

core::arch::global_asm!(include_str!(concat!(env!("OUT_DIR"), "/platform-hardware.inc")),
                        include_str!("entry.S"));

const TEXT_BYTES: usize = 200;
const MAX_CHECKS: usize = 20;
const CACHE_TEST_BYTES: usize = 4096;
const DMA_WORDS: usize = 4096;

#[panic_handler]
fn panic(_: &PanicInfo) -> !
{
    uart::puts("SELFTEST_PANIC\n");
    loop
    {
        core::hint::spin_loop();
    }
}

#[derive(Clone, Copy)]
struct Text
{
    bytes: [u8; TEXT_BYTES],
    len:   usize
}

impl Text
{
    const fn new() -> Self
    {
        Self { bytes: [0; TEXT_BYTES],
               len:   0 }
    }

    fn push_char(&mut self, character: u8)
    {
        if self.len < TEXT_BYTES
        {
            self.bytes[self.len] = character;
            self.len += 1;
        }
    }

    fn push_str(&mut self, text: &str) -> &mut Self
    {
        for byte in text.bytes()
        {
            self.push_char(byte);
        }
        self
    }

    fn push_hex(&mut self, value: u64) -> &mut Self
    {
        self.push_str("0x");
        let mut started = false;
        for shift in (0..64).step_by(4).rev()
        {
            let nibble = (value >> shift) & 0xf;
            if nibble != 0 || started || shift == 0
            {
                started = true;
                self.push_char(if nibble < 10
                               {
                                   b'0' + nibble as u8
                               }
                               else
                               {
                                   b'a' + (nibble as u8 - 10)
                               });
            }
        }
        self
    }

    fn push_dec(&mut self, mut value: u64) -> &mut Self
    {
        let mut digits = [0u8; 20];
        let mut at = digits.len();
        if value == 0
        {
            self.push_char(b'0');
            return self;
        }
        while value != 0
        {
            at -= 1;
            digits[at] = b'0' + (value % 10) as u8;
            value /= 10;
        }
        for digit in &digits[at..]
        {
            self.push_char(*digit);
        }
        self
    }

    fn as_str(&self) -> &str
    {
        core::str::from_utf8(&self.bytes[..self.len]).unwrap_or("")
    }
}

impl Write for Text
{
    fn write_str(&mut self, text: &str) -> core::fmt::Result
    {
        self.push_str(text);
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct Check
{
    name:   &'static str,
    ok:     bool,
    skip:   bool,
    reason: &'static str,
    detail: Text
}

impl Check
{
    const fn empty() -> Self
    {
        Self { name:   "",
               ok:     false,
               skip:   false,
               reason: "",
               detail: Text::new() }
    }
}

struct Report
{
    checks: [Check; MAX_CHECKS],
    count:  usize
}

impl Report
{
    const fn new() -> Self
    {
        Self { checks: [Check::empty(); MAX_CHECKS],
               count:  0 }
    }

    fn push(&mut self, name: &'static str, ok: bool, reason: &'static str) -> &mut Text
    {
        let at = self.count;
        self.count += 1;
        self.checks[at] = Check { name,
                                  ok,
                                  skip: false,
                                  reason,
                                  detail: Text::new() };
        &mut self.checks[at].detail
    }

    /// A check that cannot run because its hardware is not there. It is
    /// reported, never silently omitted.
    fn skip(&mut self, name: &'static str, reason: &'static str) -> &mut Text
    {
        let at = self.count;
        self.count += 1;
        self.checks[at] = Check { name,
                                  ok: false,
                                  skip: true,
                                  reason,
                                  detail: Text::new() };
        &mut self.checks[at].detail
    }

    fn passed(&self) -> usize
    {
        self.checks[..self.count].iter().filter(|check| check.ok).count()
    }

    fn failed(&self) -> usize
    {
        self.checks[..self.count].iter().filter(|check| !check.ok && !check.skip).count()
    }

    fn skipped(&self) -> usize
    {
        self.checks[..self.count].iter().filter(|check| check.skip).count()
    }

    /// The exact text the QR carries: one `name=ok`, `name=fail: reason` or
    /// `name=skip: reason` per line, and nothing else.
    fn payload(&self) -> Text
    {
        let mut text = Text::new();
        for (index, check) in self.checks[..self.count].iter().enumerate()
        {
            if index != 0
            {
                text.push_char(b'\n');
            }
            text.push_str(check.name);
            if check.ok
            {
                text.push_str("=ok");
            }
            else if check.skip
            {
                text.push_str("=skip: ");
                text.push_str(check.reason);
            }
            else
            {
                text.push_str("=fail: ");
                text.push_str(check.reason);
            }
        }
        text
    }

    fn log(&self)
    {
        for check in &self.checks[..self.count]
        {
            uart::puts("SELFTEST_RESULT ");
            uart::puts(check.name);
            if check.ok
            {
                uart::puts("=ok ");
            }
            else if check.skip
            {
                uart::puts("=skip: ");
                uart::puts(check.reason);
                uart::putc(b' ');
            }
            else
            {
                uart::puts("=fail: ");
                uart::puts(check.reason);
                uart::putc(b' ');
            }
            uart::puts(check.detail.as_str());
            uart::putc(b'\n');
        }
        uart::puts("SELFTEST_TOTAL ");
        uart::putdec(self.passed() as u64);
        uart::puts(" ok ");
        uart::putdec(self.failed() as u64);
        uart::puts(" fail ");
        uart::putdec(self.skipped() as u64);
        uart::puts(" skip\n");
    }
}

fn mailbox_reason(error: &Error) -> &'static str
{
    match error
    {
        Error::NoResponse => "no-response",
        Error::MailboxFull => "mailbox-full",
        Error::RequestRefused { .. } => "refused",
        Error::TagRefused { .. } => "tag-refused",
        Error::MissingTag { .. } => "missing-tag",
        Error::ShortValue { .. } => "short-reply",
        Error::ReturnedTooLong { .. } => "long-reply",
        Error::Malformed { .. } => "malformed",
        Error::SizeMismatch { .. } => "size-mismatch",
        Error::ForeignResponse { .. } => "foreign-reply",
        Error::MessageAlignment { .. } => "alignment",
        _ => "error"
    }
}

fn note_mailbox_failure(report: &mut Report, name: &'static str, subject: &str, error: &Error)
{
    let reason = mailbox_reason(error);
    uart::puts("SELFTEST_ERROR ");
    uart::puts(subject);
    uart::puts(": ");
    let mut text = Text::new();
    let _ = write!(text, "{error}");
    uart::puts(text.as_str());
    uart::putc(b'\n');
    report.push(name, false, reason).push_str(subject).push_str(": ").push_str(reason);
}

pub(crate) fn timer() -> u64
{
    // SAFETY: the system timer window comes from the platform record and the
    // counter registers are read-only.
    unsafe {
        let high = read_volatile((platform::SYSTIMER_BASE + 8) as *const u32) as u64;
        let low = read_volatile((platform::SYSTIMER_BASE + 4) as *const u32) as u64;
        (high << 32) | low
    }
}

/// The system timer counts at 1 MHz.
fn wait_microseconds(microseconds: u64)
{
    let start = timer();
    while timer().wrapping_sub(start) < microseconds
    {
        core::hint::spin_loop();
    }
}

/// Page aligned so the whole buffer is reachable through one non-cacheable
/// alias page, which is how the experiment observes memory instead of cache.
#[repr(align(4096))]
struct CacheTest([u8; CACHE_TEST_BYTES]);

static mut CACHE_TEST: CacheTest = CacheTest([0; CACHE_TEST_BYTES]);

/// The DMA buffers. They are page aligned so the cache maintenance that guards
/// each transfer covers whole lines, and static so the engine's bus address is
/// stable for the whole run.
#[repr(align(64))]
struct DmaBuffer<const WORDS: usize>([u32; WORDS]);

static mut DMA_SOURCE: DmaBuffer<DMA_WORDS> = DmaBuffer([0; DMA_WORDS]);
static mut DMA_DEST: DmaBuffer<DMA_WORDS> = DmaBuffer([0; DMA_WORDS]);
static mut DMA_SOURCE2: DmaBuffer<256> = DmaBuffer([0; 256]);
static mut DMA_DEST2: DmaBuffer<256> = DmaBuffer([0; 256]);

fn draw_frame(screen: &screen::Screen, report: &Report, page: &str)
{
    screen.fill_all(screen::GREY);
    screen.border(2, 2, screen::WHITE);
    // Four known colours at the extremes: if they land where they belong, the
    // stride and the extent agree with the firmware's own description.
    let right = screen.width() - 22;
    let bottom = screen.height() - 22;
    screen.fill_rect(6, 6, 16, 16, screen::CYAN);
    screen.fill_rect(right, 6, 16, 16, screen::YELLOW);
    screen.fill_rect(6, bottom, 16, 16, screen::GREEN);
    screen.fill_rect(right, bottom, 16, 16, screen::RED);
    screen.text(26, 8, 1, screen::WHITE, page);
    screen.centred_text(10, 2, screen::WHITE, "AARCH64 HARDWARE SELF-TEST");
    let mut summary = Text::new();
    summary.push_str("SELF TEST ")
           .push_dec(report.passed() as u64)
           .push_str(" OK ")
           .push_dec(report.failed() as u64)
           .push_str(" FAIL ")
           .push_dec(report.skipped() as u64)
           .push_str(" SKIP");
    screen.centred_text(28,
                        3,
                        if report.failed() == 0 { screen::GREEN } else { screen::RED },
                        summary.as_str());
}

fn draw_results(screen: &screen::Screen, report: &Report, qr: Option<&qr::Qr>)
{
    draw_frame(screen, report, "1/2 RESULTS");
    if let Some(qr) = qr
    {
        // The QR wins the space: the largest integer scale that fits the band
        // between the summary and the detail lines, centred on both axes.
        let available = 468 - 58;
        let scale = screen::Screen::qr_scale(qr, available);
        let panel = (qr.size + 8) * scale;
        let x = screen.width().saturating_sub(panel) / 2;
        let y = 58 + available.saturating_sub(panel) / 2;
        screen.qr(x, y, scale, qr, screen::BLACK, screen::WHITE);
    }
    else
    {
        screen.centred_text(240, 3, screen::RED, "QR ENCODING FAILED");
    }
    // Two columns keep nineteen checks readable without shrinking the QR.
    for (index, check) in report.checks[..report.count].iter().enumerate()
    {
        let mut line = Text::new();
        line.push_str(check.name);
        if check.ok
        {
            line.push_str("=ok");
        }
        else if check.skip
        {
            line.push_str("=skip: ");
            line.push_str(check.reason);
        }
        else
        {
            line.push_str("=fail: ");
            line.push_str(check.reason);
        }
        while line.len < 22
        {
            line.push_char(b' ');
        }
        line.push_str(check.detail.as_str());
        let column = index / 10;
        let row = index % 10;
        screen.text(20 + column * 390,
                    478 + row * 8,
                    1,
                    if check.ok
                    {
                        screen::WHITE
                    }
                    else if check.skip
                    {
                        screen::YELLOW
                    }
                    else
                    {
                        screen::RED
                    },
                    line.as_str());
    }
}

fn draw_cache_demo(screen: &screen::Screen, report: &Report, qr: Option<&qr::Qr>)
{
    draw_frame(screen, report, "2/2 CACHE WRITE-BACK DEMO");
    screen.centred_text(34,
                        1,
                        screen::WHITE,
                        "SAME QR, TWO PATHS: LEFT IS WRITTEN THEN CLEANED; RIGHT IS WRITTEN \
                         WITHOUT A CLEAN");
    if let Some(qr) = qr
    {
        let scale = screen::Screen::qr_scale(qr, 300);
        let panel = (qr.size + 8) * scale;
        screen.qr(60 + (300 - panel) / 2,
                  56 + (300 - panel) / 2,
                  scale,
                  qr,
                  screen::BLACK,
                  screen::WHITE);
        // The right panel is deliberately not the QR. It is the previous frame,
        // cleaned, so the uncleaned QR below has something stale to leave behind.
        screen.fill_rect(440, 56, 300, 300, screen::RED);
        screen.centred_text(150, 4, screen::WHITE, "STALE");
        screen.centred_text(200, 1, screen::WHITE, "THIS PANEL IS WHAT THE SCANOUT SEES");
    }
    screen.text(24,
                372,
                1,
                screen::WHITE,
                "LEFT: dc cvac AFTER THE WRITE - THE SCANOUT SEES THE QR.");
    screen.text(24,
                386,
                1,
                screen::WHITE,
                "RIGHT: NO CLEAN - THE SCANOUT STILL SEES THE RED PANEL.");
    screen.text(24,
                400,
                1,
                screen::WHITE,
                "DARK MODULES ON THE RIGHT ARE LINES THE CACHE EVICTED BY ITSELF.");
    let mut row = 0;
    for check in report.checks[..report.count].iter()
    {
        if check.name == "cache-clean" || check.name == "cache-dirty"
        {
            let mut line = Text::new();
            line.push_str(check.name);
            line.push_str(if check.ok { "=ok " } else { "=fail: " });
            if !check.ok
            {
                line.push_str(check.reason);
            }
            else
            {
                line.push_str(check.detail.as_str());
            }
            screen.text(24,
                        430 + row * 20,
                        2,
                        if check.ok { screen::GREEN } else { screen::RED },
                        line.as_str());
            row += 1;
        }
    }
    screen.flush();
    if let Some(qr) = qr
    {
        // Written after the clean, and never cleaned. If the cache were absent
        // the modules would appear; with a write-back cache they stay in it.
        let scale = screen::Screen::qr_scale(qr, 300);
        let panel = (qr.size + 8) * scale;
        screen.qr_modules_only(440 + (300 - panel) / 2,
                               56 + (300 - panel) / 2,
                               scale,
                               qr,
                               screen::BLACK);
    }
}

/// Append a controller failure's command index and interrupt state, so a
/// refusal on real hardware says where it stopped rather than only that it did.
fn describe_failure(text: &mut Text, failure: &mac_sdhci::Failure)
{
    if failure.command == u8::MAX
    {
        text.push_str("no command");
    }
    else
    {
        text.push_str("cmd ").push_dec(u64::from(failure.command));
    }
    text.push_str(" int ")
        .push_hex(u64::from(failure.normal))
        .push_str(" err ")
        .push_hex(u64::from(failure.error));
    if failure.caps != 0
    {
        text.push_str(" cap ").push_hex(u64::from(failure.caps));
    }
    // Only a stuck error bit is interesting: other normal-status bits are
    // events that legitimately sit in the register between commands.
    if failure.before & 0x8000 != 0
    {
        text.push_str(" pre ").push_hex(u64::from(failure.before));
    }
    if failure.completed
    {
        text.push_str(" resp ").push_hex(u64::from(failure.resp));
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_entry() -> !
{
    uart::puts("SELFTEST_START platform=");
    uart::puts(platform::PLATFORM_NAME);
    uart::putc(b'\n');

    let mut report = Report::new();
    let mut core_hz = 0u64;
    let mut storage = mailbox::Storage::new();
    let mut mailbox = mailbox::mailbox();
    let mut message = match mailbox::message(&mut storage)
    {
        Ok(message) => message,
        Err(error) =>
        {
            uart::puts("SELFTEST_FATAL message buffer: ");
            let mut text = Text::new();
            let _ = write!(text, "{error}");
            uart::puts(text.as_str());
            uart::putc(b'\n');
            loop
            {
                core::hint::spin_loop();
            }
        }
    };
    // A stale response from before this binary ran would be read as ours.
    mailbox.drain();

    // 1. Mailbox property channel: the board revision. A well-formed reply with
    // a zero value is still a refusal to describe the board, so it fails.
    match mailbox::query(&mut message, &mut mailbox, mailbox::TAG_BOARD_REVISION, &[0])
    {
        Ok([0, _]) =>
        {
            report.push("mailbox", false, "zero-revision").push_str("board revision is zero");
        }
        Ok([value, _]) =>
        {
            report.push("mailbox", true, "").push_str("board ").push_hex(u64::from(value));
        }
        Err(error) => note_mailbox_failure(&mut report, "mailbox", "board revision", &error)
    }

    // 2. The firmware revision, same channel.
    match mailbox::query(&mut message, &mut mailbox, mailbox::TAG_FIRMWARE_REVISION, &[0])
    {
        Ok([0, _]) =>
        {
            report.push("fw-rev", false, "zero-revision").push_str("firmware revision is zero");
        }
        Ok([value, _]) =>
        {
            report.push("fw-rev", true, "").push_str("firmware ").push_hex(u64::from(value));
        }
        Err(error) => note_mailbox_failure(&mut report, "fw-rev", "firmware revision", &error)
    }

    // 3. The core clock. The reply echoes the clock id and then states the
    // rate, so the rate is the second word. A plausible nonzero value is the
    // whole check: this is a query, not a measurement, and a zero means the
    // firmware did not answer.
    match mailbox::query(&mut message,
                         &mut mailbox,
                         mailbox::TAG_GET_CLOCK_RATE,
                         &[mailbox::CLOCK_CORE, 0])
    {
        Ok([_, 0]) =>
        {
            report.push("clock", false, "zero-clock")
                  .push_str("core clock id ")
                  .push_dec(u64::from(mailbox::CLOCK_CORE))
                  .push_str(" returned zero");
        }
        Ok([id, rate]) =>
        {
            core_hz = u64::from(rate);
            report.push("clock", true, "")
                  .push_str("core id ")
                  .push_dec(u64::from(id))
                  .push_str(" rate ")
                  .push_dec(u64::from(rate))
                  .push_str(" hz");
        }
        Err(error) => note_mailbox_failure(&mut report, "clock", "core clock", &error)
    }

    // 4. The ARM memory size against the record's declared RAM. The reply is
    // the ARM base followed by the size. Firmware reserves memory for the
    // VideoCore, so a shortfall is expected; a value outside the declared part,
    // or one that is grossly short, is not.
    match mailbox::query(&mut message, &mut mailbox, mailbox::TAG_GET_ARM_MEMORY, &[0, 0])
    {
        Ok([_, 0]) =>
        {
            report.push("memory", false, "zero-memory").push_str("firmware reported no RAM");
        }
        Ok([_, bytes]) if u64::from(bytes) > platform::DECLARED_RAM_BYTES =>
        {
            report.push("memory", false, "over-ram")
                  .push_str("reported ")
                  .push_hex(u64::from(bytes))
                  .push_str(" above declared ")
                  .push_hex(platform::DECLARED_RAM_BYTES);
        }
        Ok([_, bytes]) if u64::from(bytes) < platform::DECLARED_RAM_BYTES / 2 =>
        {
            report.push("memory", false, "short-ram")
                  .push_str("reported ")
                  .push_hex(u64::from(bytes))
                  .push_str(" under half of ")
                  .push_hex(platform::DECLARED_RAM_BYTES);
        }
        Ok([base, bytes]) =>
        {
            report.push("memory", true, "")
                  .push_str("arm ")
                  .push_hex(u64::from(bytes))
                  .push_str(" at ")
                  .push_hex(u64::from(base))
                  .push_str(" of ")
                  .push_hex(platform::DECLARED_RAM_BYTES);
        }
        Err(error) => note_mailbox_failure(&mut report, "memory", "arm memory", &error)
    }

    // 5. The framebuffer. The base is the masked bus address; the pitch is the
    // firmware's own answer, not width x depth.
    let mode = Mode { width:       platform::DISPLAY_WIDTH,
                      height:      platform::DISPLAY_HEIGHT,
                      depth:       32,
                      pixel_order: PixelOrder::Bgr };
    let mut framebuffer: Option<Framebuffer> = None;
    match acquire(&mut mailbox, &mut message, mode, 4096)
    {
        Ok(fb) =>
        {
            let aligned = fb.base % 4 == 0;
            let start = u64::from(fb.base);
            let end = start + u64::from(fb.size);
            let inside = start >= platform::DECLARED_RAM_BASE
                         && end <= platform::DECLARED_RAM_BASE + platform::DECLARED_RAM_BYTES;
            if aligned && inside
            {
                report.push("fb-acquire", true, "")
                      .push_str("base ")
                      .push_hex(u64::from(fb.base))
                      .push_str(" size ")
                      .push_hex(u64::from(fb.size))
                      .push_str(" pitch ")
                      .push_hex(u64::from(fb.pitch));
                framebuffer = Some(fb);
            }
            else
            {
                report.push("fb-acquire", false, if aligned { "out-of-ram" } else { "unaligned" })
                      .push_str("base ")
                      .push_hex(u64::from(fb.base))
                      .push_str(" size ")
                      .push_hex(u64::from(fb.size));
            }
        }
        Err(error) => note_mailbox_failure(&mut report, "fb-acquire", "framebuffer", &error)
    }

    // The screen goes live here, before any of the remaining tests. Grey is
    // deliberately not black: a grey screen with no QR means the framebuffer
    // and the drawing worked but the encoder did not, which is a different
    // fault from a black screen where the scanout never came up.
    if let Some(fb) = framebuffer
    {
        let screen = screen::Screen::new(&fb);
        screen.fill_all(screen::GREY);
        screen.border(2, 2, screen::WHITE);
        screen.flush();
        uart::puts("SELFTEST_SCREEN grey\n");
    }

    // 6. Framebuffer write/read-back through the masked base at scattered
    // offsets. This is the address test; the cache is still off.
    if let Some(fb) = framebuffer
    {
        let offsets = [0usize, fb.size as usize / 4, fb.size as usize / 2, fb.size as usize - 4];
        let mut mismatches = 0usize;
        for (index, offset) in offsets.iter().enumerate()
        {
            let address = fb.base as usize + offset;
            let value = 0xa5a5_0000u32 | index as u32;
            // SAFETY: each offset is inside the allocation and four-byte aligned.
            unsafe {
                write_volatile(address as *mut u32, value);
                if read_volatile(address as *const u32) != value
                {
                    mismatches += 1;
                }
            }
        }
        if mismatches == 0
        {
            report.push("fb-rw", true, "")
                  .push_str("4 offsets read back at base ")
                  .push_hex(u64::from(fb.base));
        }
        else
        {
            report.push("fb-rw", false, "mismatch")
                  .push_dec(mismatches as u64)
                  .push_str(" of 4 offsets did not read back");
        }
    }
    else
    {
        report.push("fb-rw", false, "no-framebuffer").push_str("framebuffer was not acquired");
    }

    // 7. SDHCI, read-only: sector zero's MBR signature and partition entries.
    let mut stage = 0u8;
    match Disk::probe(platform::SDHCI_BASE, &mut stage)
    {
        Ok(mut disk) =>
        {
            match disk.read_block(0)
            {
                Ok(sector) =>
                {
                    let signature = [sector[510], sector[511]];
                    if signature == [0x55, 0xaa]
                    {
                        let mut detail = Text::new();
                        detail.push_str("mbr 0x55aa");
                        for entry in mbr::entries(&sector).iter().flatten()
                        {
                            detail.push_str(" p")
                                  .push_dec(entry.index as u64)
                                  .push_str(" lba ")
                                  .push_dec(u64::from(entry.first))
                                  .push_str(" type ")
                                  .push_hex(u64::from(entry.kind));
                        }
                        report.push("sdhci", true, "").push_str(detail.as_str());
                    }
                    else
                    {
                        // Distinguishing "the transfer produced nothing" from
                        // "the transfer produced the wrong bytes" decides
                        // whether to look at the controller or the card.
                        let mut nonzero = 0u32;
                        for byte in sector.iter()
                        {
                            if *byte != 0
                            {
                                nonzero += 1;
                            }
                        }
                        let head = u32::from_be_bytes([sector[0], sector[1], sector[2], sector[3]]);
                        let (present, normal, error) = disk.after_read();
                        report.push("sdhci", false, "no-mbr")
                              .push_str("signature ")
                              .push_hex(u64::from(signature[0]) << 8 | u64::from(signature[1]))
                              .push_str(" nz ")
                              .push_dec(u64::from(nonzero))
                              .push_str(" head ")
                              .push_hex(u64::from(head))
                              .push_str(" ps ")
                              .push_hex(u64::from(present))
                              .push_str(" int ")
                              .push_hex(u64::from(normal))
                              .push_str(" err ")
                              .push_hex(u64::from(error));
                    }
                }
                Err(failure) =>
                {
                    let mut detail = Text::new();
                    detail.push_str("sector 0 read: ");
                    describe_failure(&mut detail, &failure);
                    report.push("sdhci", false, failure.what).push_str(detail.as_str());
                }
            }
        }
        Err(failure) =>
        {
            let mut detail = Text::new();
            detail.push_str("bring-up stage ").push_dec(u64::from(stage)).push_str(": ");
            describe_failure(&mut detail, &failure);
            report.push("sdhci", false, failure.what).push_str(detail.as_str());
        }
    }

    // 8. The system timer advances.
    let start = timer();
    let mut spins = 0u32;
    while timer() == start && spins < 5_000_000
    {
        spins += 1;
        core::hint::spin_loop();
    }
    let end = timer();
    if end != start
    {
        report.push("timer", true, "").push_str("advanced ").push_dec(end.wrapping_sub(start));
    }
    else
    {
        report.push("timer", false, "stalled").push_str("counter did not advance");
    }

    // 9. Turn translation and caches on. The mailbox is no longer used after
    // this point: its message buffer would need cleaning before the firmware
    // could read it again, and this program has nothing left to ask.
    //
    // The experiment's buffer is addressed first, while translation is still
    // off, so its address is its physical address too; the page table then
    // gives that page a second, non-cacheable VA.
    // SAFETY: a static owned by this single-threaded run; taking its address
    // does not create a reference.
    let buffer = unsafe { core::ptr::addr_of_mut!(CACHE_TEST.0) as *mut u8 };
    let address = buffer as usize;
    let mapped = mmu::enable(address);
    let enabled = mapped && mmu::enabled();
    report.push("mmu",
                enabled,
                if mapped { "not-enabled" } else { "record" })
          .push_str(if enabled
          {
              "identity 39-bit, caches on"
          }
          else
          {
              "translation or caches did not turn on"
          });

    // 10. Cache coherence, the reason this binary exists. Write a pattern,
    // clean it to the point of coherency, then read it back through the
    // non-cacheable alias: the clean is what put it in memory. Then write a
    // second pattern and read again without cleaning: memory must still hold
    // the first. Reading through the cache would answer from the cache in both
    // halves and prove nothing, and invalidating would not help, because on
    // Cortex-A53 `dc ivac` cleans a dirty line before it invalidates it.
    // SAFETY: the alias maps this buffer's page, so both pointers address
    // memory this run owns.
    let view = mmu::ALIAS_BASE as *const u8;
    for index in 0..CACHE_TEST_BYTES
    {
        // SAFETY: `index` is inside the buffer.
        unsafe { write_volatile(buffer.add(index), 0xa5) };
    }
    cache::clean(address, CACHE_TEST_BYTES);
    let mut clean_ok = true;
    let mut clean_observed = 0u8;
    for index in 0..CACHE_TEST_BYTES
    {
        // SAFETY: as above; non-cacheable, so this is memory rather than cache.
        let value = unsafe { read_volatile(view.add(index)) };
        if index == 0
        {
            clean_observed = value;
        }
        if value != 0xa5
        {
            clean_ok = false;
        }
    }
    if clean_ok
    {
        report.push("cache-clean", true, "").push_str("cleaned read back 0xa5 at every byte");
    }
    else
    {
        report.push("cache-clean", false, "short-clean")
              .push_str("first byte ")
              .push_hex(u64::from(clean_observed));
    }

    for index in 0..CACHE_TEST_BYTES
    {
        // SAFETY: `index` is inside the buffer.
        unsafe { write_volatile(buffer.add(index), 0x5a) };
    }
    // The second pattern is never cleaned, so a write-back cache leaves the
    // first pattern in memory, where the alias sees it.
    // SAFETY: as above.
    let dirty_observed = unsafe { read_volatile(view) };
    // The stale byte must be the first pattern: that is what proves the clean
    // above really reached DRAM and that this line was dirty. Anything else
    // means the two halves disagree about where the bytes live.
    let drift = dirty_observed == 0xa5;
    if drift
    {
        report.push("cache-dirty", true, "")
              .push_str("uncleaned write read back 0xa5, the cleaned pattern");
    }
    else
    {
        let reason = if dirty_observed == 0x5a { "no-drift" } else { "unexpected" };
        report.push("cache-dirty", false, reason)
              .push_str("uncleaned write read back ")
              .push_hex(u64::from(dirty_observed))
              .push_str("; wanted 0xa5");
    }

    // 12. SPI0: the block is live, the divider reads back, and the transmit
    // status bit is produced by the FIFO rather than stored by the CPU.
    let mut spi = spi0::Spi0::new();
    let mut spi_ready = false;
    let mut shift = 0u32;
    if spi.present()
    {
        match spi.claim()
        {
            Ok(()) =>
            {
                report.push("spi-claim", true, "").push_str("clk 1000 txd ready");
                spi_ready = true;
            }
            Err(reason) =>
            {
                report.push("spi-claim", false, reason).push_str("claim: ").push_str(reason);
            }
        }
    }
    else
    {
        report.skip("spi-claim", "no-block").push_str("SPI0 registers did not read back");
    }

    // 13. Byte length: send 32 bytes and count what came back. The count is
    // also what reveals whether a FIFO entry shifts 8 bits or 32, which is the
    // fact the "n words is 4n bytes" rule depends on.
    if spi_ready
    {
        match spi.probe_shift()
        {
            Ok(received) =>
            {
                shift = spi.shift_bits();
                let (ok, reason) = match shift
                {
                    32 => (true, ""),
                    8 => (false, "shift-8"),
                    _ => (false, "unknown")
                };
                report.push("spi-len", ok, reason)
                      .push_str("dlen 32 rx ")
                      .push_dec(received as u64)
                      .push_str(" shift ")
                      .push_dec(u64::from(shift));
            }
            Err(reason) =>
            {
                report.push("spi-len", false, reason).push_str("probe: ").push_str(reason);
            }
        }
    }
    else
    {
        report.skip("spi-len", "no-claim").push_str("SPI0 was not claimed");
    }

    // 14. A non-power-of-two divider against a power of two. The ratio cancels
    // the core clock, so a rounded divisor shows as a ratio of 1.0 instead of
    // 1.024 even if the rate itself is unknown.
    if shift != 0
    {
        match (spi.transfer(256, 1000), spi.transfer(256, 1024))
        {
            (Ok((_, slow)), Ok((_, fast))) if slow != 0 && fast != 0 =>
            {
                let ratio = fast.saturating_mul(1_000_000) / slow;
                let off = ratio.abs_diff(1_024_000);
                let mut detail = Text::new();
                detail.push_str("1000/1024 ratio ").push_dec(ratio);
                if core_hz != 0
                {
                    detail.push_str(" core ").push_dec(core_hz);
                }
                if off <= 10_240
                {
                    report.push("spi-div", true, "").push_str(detail.as_str());
                }
                else if ratio.abs_diff(1_000_000) <= 10_240
                {
                    report.push("spi-div", false, "rounded")
                          .push_str(detail.as_str())
                          .push_str("; non-power-of-two rounded");
                }
                else
                {
                    report.push("spi-div", false, "rate").push_str(detail.as_str());
                }
            }
            (Err(reason), _) | (_, Err(reason)) =>
            {
                report.push("spi-div", false, reason).push_str("transfer: ").push_str(reason);
            }
            _ =>
            {
                report.push("spi-div", false, "timer").push_str("no timer advance");
            }
        }
    }
    else
    {
        report.skip("spi-div", "no-shift").push_str("the shift width was not established");
    }

    // 15. The DMA controller: presence, then a channel from the board's ARM
    // set. The mask is the record's pool, never all fifteen.
    let dma = dma::Dma::new();
    let dma_present = dma.present();
    let channel = if dma_present { dma.claim(platform::DMA_POOL) } else { None };
    let channel_ok = channel.is_some();

    // 16-17. Pacing and completion: one DMA-fed SPI transfer, measured against
    // a CPU-fed transfer of the same byte count. Completion is the SPI's own
    // done bit, not the engine's active bit.
    let mut spi_dma_ok = false;
    let mut spi_dma_reason: &'static str = "no-dma";
    let mut spi_busy = false;
    let mut spi_dma_duration = 0u64;
    let mut spi_cpu_duration = 0u64;
    if spi_ready && shift != 0 && channel_ok
    {
        let bytes = 512usize;
        let dma_bytes = if shift == 8 { bytes * 4 } else { bytes };
        // SAFETY: a static owned by this single-threaded run.
        let source = unsafe { core::ptr::addr_of_mut!(DMA_SOURCE.0) as *mut u32 };
        for index in 0..dma_bytes / 4
        {
            // SAFETY: `index` is inside the buffer.
            unsafe { write_volatile(source.add(index), 0xa5a5_0000 | index as u32) };
        }
        match spi.transfer(bytes, 1000)
        {
            Ok((_, duration)) => spi_cpu_duration = duration,
            Err(reason) => spi_dma_reason = reason
        }
        if let Some(ch) = channel.as_ref()
        {
            match spi.prepare_dma(bytes, 1000)
            {
                Ok(()) =>
                {
                    let block = dma::ControlBlock::spi_write(source as usize,
                                                              spi0::FIFO_BUS,
                                                              dma_bytes);
                    cache::clean(source as usize, dma_bytes);
                    let start = timer();
                    match ch.start(&block)
                    {
                        Ok(()) =>
                        {
                            // The engine is running before the block is told to
                            // shift; the other order waits on an empty FIFO.
                            spi.start();
                            let dma_result = ch.wait();
                            spi_busy = spi.busy();
                            let spi_result = spi.wait_done();
                            let end = timer();
                            spi.finish();
                            spi_dma_duration = end.wrapping_sub(start);
                            spi_dma_ok = dma_result.is_ok()
                                         && spi_result.is_ok()
                                         && spi_cpu_duration != 0;
                            if let Err(reason) = dma_result
                            {
                                spi_dma_reason = reason;
                            }
                            else if let Err(reason) = spi_result
                            {
                                spi_dma_reason = reason;
                            }
                        }
                        Err(reason) =>
                        {
                            spi.finish();
                            spi_dma_reason = reason;
                        }
                    }
                }
                Err(reason) => spi_dma_reason = reason
            }
        }
    }
    if !spi_ready
    {
        report.skip("spi-pace", "no-claim").push_str("SPI0 was not claimed");
        report.skip("spi-done", "no-claim").push_str("SPI0 was not claimed");
    }
    else if shift == 0
    {
        report.skip("spi-pace", "no-shift").push_str("the shift width was not established");
        report.skip("spi-done", "no-shift").push_str("the shift width was not established");
    }
    else if !channel_ok
    {
        report.skip("spi-pace", "no-dma").push_str("no DMA channel from the ARM set");
        report.skip("spi-done", "no-dma").push_str("no DMA channel from the ARM set");
    }
    else if spi_dma_ok
    {
        let off = spi_dma_duration.abs_diff(spi_cpu_duration);
        let within = off * 100 <= spi_cpu_duration * 2;
        if within
        {
            report.push("spi-pace", true, "")
                  .push_str("dma ")
                  .push_dec(spi_dma_duration)
                  .push_str(" us cpu ")
                  .push_dec(spi_cpu_duration)
                  .push_str(" us");
        }
        else
        {
            report.push("spi-pace", false, "gap")
                  .push_str("dma ")
                  .push_dec(spi_dma_duration)
                  .push_str(" us cpu ")
                  .push_dec(spi_cpu_duration)
                  .push_str(" us");
        }
        report.push("spi-done",
                    true,
                    "")
              .push_str(if spi_busy
              {
                  "dma done, spi still busy"
              }
              else
              {
                  "spi done with dma"
              });
    }
    else
    {
        report.push("spi-pace", false, spi_dma_reason)
              .push_str("dma-fed transfer: ")
              .push_str(spi_dma_reason);
        report.push("spi-done", false, spi_dma_reason)
              .push_str("dma-fed transfer: ")
              .push_str(spi_dma_reason);
    }

    // 18. A channel lease on real hardware: a memory-to-memory copy whose
    // source and destination are both in the buffer the CPU owns.
    if !dma_present
    {
        report.skip("dma-lease", "no-dma").push_str("DMA registers did not read back");
        report.skip("dma-2nd", "no-dma").push_str("DMA registers did not read back");
        report.skip("dma-clean", "no-dma").push_str("DMA registers did not read back");
    }
    else if let Some(ch) = channel.as_ref()
    {
        let words = 1024usize;
        // SAFETY: statics owned by this single-threaded run.
        let source = unsafe { core::ptr::addr_of_mut!(DMA_SOURCE.0) as *mut u32 };
        let dest = unsafe { core::ptr::addr_of_mut!(DMA_DEST.0) as *mut u32 };
        for index in 0..words
        {
            // SAFETY: `index` is inside the buffer.
            unsafe { write_volatile(source.add(index), 0x1122_3344 ^ index as u32) };
        }
        match ch.copy(source as usize, dest as usize, words * 4, true)
        {
            Ok(()) =>
            {
                cache::invalidate(dest as usize, words * 4);
                let mut landed = true;
                for index in 0..words
                {
                    // SAFETY: as above.
                    if unsafe { read_volatile(dest.add(index)) } != (0x1122_3344 ^ index as u32)
                    {
                        landed = false;
                        break;
                    }
                }
                if landed
                {
                    report.push("dma-lease", true, "")
                          .push_str("ch ")
                          .push_dec(u64::from(ch.number()))
                          .push_str(" 4096 bytes");
                }
                else
                {
                    report.push("dma-lease", false, "mismatch")
                          .push_str("the copy did not land");
                }
            }
            Err(reason) =>
            {
                report.push("dma-lease", false, reason).push_str("copy: ").push_str(reason);
            }
        }

        // 19. A second acquisition of the same channel while the first is in
        // flight. The controller has no ownership check; the observation is
        // whether the first transfer survives it.
        let source2 = unsafe { core::ptr::addr_of_mut!(DMA_SOURCE2.0) as *mut u32 };
        let dest2 = unsafe { core::ptr::addr_of_mut!(DMA_DEST2.0) as *mut u32 };
        let long_words = DMA_WORDS;
        for index in 0..long_words
        {
            // SAFETY: `index` is inside the buffer.
            unsafe { write_volatile(source.add(index), 0x5566_7788 ^ index as u32) };
        }
        for index in 0..256
        {
            // SAFETY: `index` is inside the second buffer.
            unsafe { write_volatile(source2.add(index), 0x99aa_bbcc ^ index as u32) };
        }
        cache::clean(source as usize, long_words * 4);
        cache::clean(source2 as usize, 256 * 4);
        cache::invalidate(dest as usize, long_words * 4);
        let first = dma::ControlBlock::memory_copy(source as usize,
                                                   dest as usize,
                                                   long_words * 4);
        match ch.start(&first)
        {
            Ok(()) =>
            {
                let busy = ch.active();
                let second = dma::ControlBlock::memory_copy(source2 as usize,
                                                            dest2 as usize,
                                                            256 * 4);
                let second_start = ch.start(&second);
                let wait = ch.wait();
                cache::invalidate(dest as usize, long_words * 4);
                // SAFETY: `dest` is inside the buffer.
                let first_landed = unsafe { read_volatile(dest) } == 0x5566_7788;
                if busy && wait.is_ok() && first_landed
                {
                    report.push("dma-2nd", true, "")
                          .push_str("busy seen; second ")
                          .push_str(if second_start.is_ok() { "accepted" } else { "refused" })
                          .push_str(", first intact");
                }
                else if !busy
                {
                    report.push("dma-2nd", false, "too-fast")
                          .push_str("the first transfer finished before it was observed");
                }
                else if !first_landed
                {
                    report.push("dma-2nd", false, "corrupt")
                          .push_str("the second acquisition disturbed the first");
                }
                else
                {
                    let reason = match wait
                    {
                        Err(reason) => reason,
                        Ok(()) => "second"
                    };
                    report.push("dma-2nd", false, reason)
                          .push_str("first transfer did not complete");
                }
            }
            Err(reason) =>
            {
                report.push("dma-2nd", false, reason).push_str("start: ").push_str(reason);
            }
        }

        // 20. The clean the document requires. The cleaned copy must land; the
        // uncleaned one is the observation, not a failure: it says whether the
        // requirement is visible on this silicon.
        for index in 0..words
        {
            // SAFETY: `index` is inside the buffer.
            unsafe { write_volatile(source.add(index), 0xa5a5_a5a5) };
        }
        match ch.copy(source as usize, dest as usize, words * 4, true)
        {
            Ok(()) =>
            {
                cache::invalidate(dest as usize, words * 4);
                let mut cleaned = true;
                for index in 0..words
                {
                    // SAFETY: as above.
                    if unsafe { read_volatile(dest.add(index)) } != 0xa5a5_a5a5
                    {
                        cleaned = false;
                        break;
                    }
                }
                for index in 0..words
                {
                    // SAFETY: as above.
                    unsafe { write_volatile(source.add(index), 0x5a5a_5a5a) };
                }
                let uncleaned = ch.copy(source as usize, dest as usize, words * 4, false);
                cache::invalidate(dest as usize, words * 4);
                // SAFETY: `dest` is inside the buffer.
                let observed = unsafe { read_volatile(dest) };
                if !cleaned
                {
                    report.push("dma-clean", false, "clean")
                          .push_str("the cleaned copy did not land");
                }
                else if let Err(reason) = uncleaned
                {
                    report.push("dma-clean", false, reason)
                          .push_str("uncleaned copy: ")
                          .push_str(reason);
                }
                else
                {
                    let note = if observed == 0xa5a5_a5a5
                    {
                        " clean required"
                    }
                    else if observed == 0x5a5a_5a5a
                    {
                        " clean not observable"
                    }
                    else
                    {
                        ""
                    };
                    report.push("dma-clean", true, "")
                          .push_str("unclean read ")
                          .push_hex(u64::from(observed))
                          .push_str(note);
                }
            }
            Err(reason) =>
            {
                report.push("dma-clean", false, reason).push_str("copy: ").push_str(reason);
            }
        }
    }
    else
    {
        report.push("dma-lease", false, "no-channel")
              .push_str("no channel from the ARM set");
        report.push("dma-2nd", false, "no-channel")
              .push_str("no channel from the ARM set");
        report.push("dma-clean", false, "no-channel")
              .push_str("no channel from the ARM set");
    }

    report.log();
    let payload = report.payload();
    uart::puts("SELFTEST_QR_BEGIN\n");
    uart::puts(payload.as_str());
    uart::puts("\nSELFTEST_QR_END\n");
    let encoded = qr::encode(payload.as_str().as_bytes());
    if encoded.is_none()
    {
        uart::puts("SELFTEST_QR_FAILED\n");
    }

    match framebuffer
    {
        Some(fb) =>
        {
            let screen = screen::Screen::new(&fb);
            loop
            {
                draw_results(&screen, &report, encoded.as_ref());
                screen.flush();
                uart::puts("SELFTEST_SCREEN results\n");
                wait_microseconds(20_000_000);
                draw_cache_demo(&screen, &report, encoded.as_ref());
                uart::puts("SELFTEST_SCREEN demo\n");
                wait_microseconds(10_000_000);
            }
        }
        None =>
        {
            uart::puts("SELFTEST_NO_SCREEN: framebuffer was never acquired\n");
            loop
            {
                core::hint::spin_loop();
            }
        }
    }
}
