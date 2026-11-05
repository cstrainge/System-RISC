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

use mac_videocore_fb::{Error, Framebuffer, Mode, PixelOrder, acquire};

mod cache;
mod font;
mod mailbox;
mod mmu;
mod platform;
mod qr;
mod screen;
mod sdhci;
mod uart;

core::arch::global_asm!(include_str!(concat!(env!("OUT_DIR"), "/platform-hardware.inc")),
                        include_str!("entry.S"));

const TEXT_BYTES: usize = 200;
const MAX_CHECKS: usize = 12;
const CACHE_TEST_BYTES: usize = 4096;

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
    reason: &'static str,
    detail: Text
}

impl Check
{
    const fn empty() -> Self
    {
        Self { name:   "",
               ok:     false,
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
        self.count - self.passed()
    }

    /// The exact text the QR carries: one `name=ok` or `name=fail: reason` per
    /// line, and nothing else.
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
        uart::puts(" fail\n");
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

fn timer() -> u64
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

#[repr(align(64))]
struct CacheTest([u8; CACHE_TEST_BYTES]);

static mut CACHE_TEST: CacheTest = CacheTest([0; CACHE_TEST_BYTES]);

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
           .push_str(" FAIL");
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
        let available = 505 - 58;
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
    for (index, check) in report.checks[..report.count].iter().enumerate()
    {
        let mut line = Text::new();
        line.push_str(check.name);
        if check.ok
        {
            line.push_str("=ok");
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
        screen.text(20,
                    492 + index * 8,
                    1,
                    if check.ok { screen::WHITE } else { screen::RED },
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

#[unsafe(no_mangle)]
pub extern "C" fn rust_entry() -> !
{
    uart::puts("SELFTEST_START platform=");
    uart::puts(platform::PLATFORM_NAME);
    uart::putc(b'\n');

    let mut report = Report::new();
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
    match sdhci::Disk::probe(platform::SDHCI_BASE)
    {
        Ok(mut disk) =>
        {
            match disk.read_sector_zero()
            {
                Ok(sector) =>
                {
                    let (signature, entries) = sdhci::partitions(&sector);
                    if signature == [0x55, 0xaa]
                    {
                        let mut detail = Text::new();
                        detail.push_str("mbr 0x55aa");
                        for entry in entries.iter().flatten()
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
                        report.push("sdhci", false, "no-mbr")
                              .push_str("signature ")
                              .push_hex(u64::from(signature[0]) << 8 | u64::from(signature[1]));
                    }
                }
                Err(reason) =>
                {
                    report.push("sdhci", false, reason)
                          .push_str("sector 0 read: ")
                          .push_str(reason);
                }
            }
        }
        Err(reason) =>
        {
            report.push("sdhci", false, reason).push_str("card: ").push_str(reason);
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
    let mapped = mmu::enable();
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
    // clean it to the point of coherency, invalidate, read back: the clean is
    // what made the read correct. Then write a second pattern and invalidate
    // without cleaning: the read must return the first pattern. Both halves are
    // reported separately so the mechanism is visible, not just a tick.
    // SAFETY: the buffer is a static owned by this single-threaded run; taking
    // its address here does not create a reference.
    let buffer = unsafe { core::ptr::addr_of_mut!(CACHE_TEST.0) as *mut u8 };
    let address = buffer as usize;
    for index in 0..CACHE_TEST_BYTES
    {
        // SAFETY: `index` is inside the buffer.
        unsafe { write_volatile(buffer.add(index), 0xa5) };
    }
    cache::clean(address, CACHE_TEST_BYTES);
    cache::clean_invalidate(address, CACHE_TEST_BYTES);
    let mut clean_ok = true;
    let mut clean_observed = 0u8;
    for index in 0..CACHE_TEST_BYTES
    {
        // SAFETY: as above; the volatile read defeats any cached register copy.
        let value = unsafe { read_volatile(buffer.add(index)) };
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
    // The second pattern is never cleaned. Invalidate discards it instead of
    // writing it back, so a real cache leaves the first pattern in DRAM.
    cache::invalidate(address, CACHE_TEST_BYTES);
    // SAFETY: the buffer is a static owned by this run.
    let dirty_observed = unsafe { read_volatile(buffer) };
    let drift = dirty_observed != 0x5a;
    if drift
    {
        report.push("cache-dirty", true, "")
              .push_str("uncleaned write read back ")
              .push_hex(u64::from(dirty_observed))
              .push_str(", wanted 0x5a");
    }
    else
    {
        report.push("cache-dirty", false, "no-drift")
              .push_str("uncleaned write read back 0x5a; the cache is not doing anything");
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
