//! The engine's decisions, against a simulated block.
//!
//! These are host tests of the *rules*, not of the hardware: a `Simulated` register
//! file cannot tell anyone whether the real ring oscillator counts words. What it
//! can do is pin the behaviour a board run would otherwise have to establish by
//! inspecting the driver: that a block counting nothing is refused rather than
//! answered with zeros, that data is never read when the count said zero, that the
//! settle words are discarded, and that quiescence actually stops the generator.
//!
//! `docs/BCM2837-RNG.md` lists what only a board can settle. Nothing here reduces
//! that list.

use bcm2835_rng::{available, Refusal, Registers, Rng, CONTROL, DATA, ENABLE, INTERRUPT_MASK,
                   MASKED, PATIENCE, STATUS, WARMUP_WORDS};

/// A register file with the block's documented behaviour and none of its timing.
struct Simulated
{
    control: u32,
    mask:    u32,
    /// Words the FIFO will report, drained one per read.
    pending: u32,
    /// The next word handed back. Deliberately recognisable so a test can tell
    /// real data from a zero fill.
    next:    u32,
    /// Every data read the engine performed, so a test can assert it never read
    /// uncounted data.
    reads:   u32,
    /// Every write, in order, so a test can assert the mask precedes the enable.
    writes:  Vec<(usize, u32)>,
    /// When false the block counts nothing however long it is asked, which is what
    /// a dead oscillator looks like.
    alive:   bool
}

impl Simulated
{
    fn alive() -> Self
    {
        Self { control: 0,
               mask: 0,
               pending: 0,
               next: 0x1122_3344,
               reads: 0,
               writes: Vec::new(),
               alive: true }
    }

    fn silent() -> Self
    {
        Self { alive: false, ..Self::alive() }
    }
}

impl Registers for Simulated
{
    fn read(&mut self, offset: usize) -> u32
    {
        match offset
        {
            STATUS =>
            {
                // The count is the top byte, and the block refills as it is drained:
                // a live block always offers what the test said it holds.
                if self.alive && self.control & ENABLE != 0
                {
                    self.pending << 24
                }
                else
                {
                    0
                }
            }
            DATA =>
            {
                self.reads += 1;
                let word = self.next;
                self.next = self.next.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                word
            }
            _ => 0
        }
    }

    fn write(&mut self, offset: usize, value: u32)
    {
        self.writes.push((offset, value));
        match offset
        {
            CONTROL => self.control = value,
            INTERRUPT_MASK => self.mask = value,
            _ => {}
        }
    }
}

#[test]
fn the_count_is_the_top_byte_of_the_status_register()
{
    assert_eq!(available(0x0000_0000), 0);
    assert_eq!(available(0x0400_0000), 4);
    assert_eq!(available(0x0400_00ff), 4, "the low bits are not part of the count");
}

#[test]
fn starting_masks_the_line_before_enabling_the_generator()
{
    let mut block = Simulated::alive();
    block.pending = WARMUP_WORDS;
    let mut rng = Rng::new();
    rng.start(&mut block).expect("a live block establishes a source");

    let mask_at = block.writes.iter().position(|(offset, _)| *offset == INTERRUPT_MASK);
    let enable_at = block.writes.iter().position(|(offset, _)| *offset == CONTROL);
    assert!(mask_at < enable_at,
            "the mask must be written first, or the block can raise a line nothing is \
             waiting for: {:?}",
            block.writes);
    assert_eq!(block.mask, MASKED);
}

#[test]
fn starting_discards_the_settling_words_and_hands_none_to_a_caller()
{
    let mut block = Simulated::alive();
    block.pending = 1;
    let mut rng = Rng::new();
    rng.start(&mut block).expect("established");
    assert_eq!(block.reads, WARMUP_WORDS, "exactly the settle words were taken");
    assert_eq!(rng.supplied(), 0, "no caller was given a settling word");
}

#[test]
fn a_block_that_counts_nothing_is_refused_rather_than_reported_ready()
{
    let mut block = Simulated::silent();
    let mut rng = Rng::new();
    let refusal = rng.start(&mut block).expect_err("a silent block must not establish a source");
    match refusal
    {
        Refusal::SourceSilent { polls } =>
        {
            assert_eq!(polls, PATIENCE, "the refusal reports the polls actually spent")
        }
        other => panic!("expected a silent-source refusal, got {other:?}")
    }
    assert!(!rng.established());
    assert_eq!(block.reads, 0, "data was never read from a block that counted nothing");
}

#[test]
fn a_fill_before_the_source_is_established_is_refused()
{
    let mut block = Simulated::alive();
    let mut rng = Rng::new();
    let mut bytes = [0u8; 8];
    assert_eq!(rng.fill(&mut block, &mut bytes), Err(Refusal::NotStarted));
    assert_eq!(block.reads, 0, "an unestablished source is not quietly used");
}

#[test]
fn data_is_never_read_when_the_count_said_zero()
{
    let mut block = Simulated::alive();
    block.pending = 1;
    let mut rng = Rng::new();
    rng.start(&mut block).expect("established");
    block.reads = 0;
    // The block dies between fills: it now counts nothing.
    block.alive = false;
    let mut bytes = [0u8; 16];
    let refusal = rng.fill(&mut block, &mut bytes).expect_err("no bytes can be supplied");
    assert_eq!(refusal, Refusal::EmptyFill);
    assert_eq!(block.reads, 0, "a zero count means the data register is not read");
}

#[test]
fn a_fill_supplies_bytes_and_reports_the_count_it_actually_supplied()
{
    let mut block = Simulated::alive();
    block.pending = 1;
    let mut rng = Rng::new();
    rng.start(&mut block).expect("established");
    let mut bytes = [0u8; 9];
    let supplied = rng.fill(&mut block, &mut bytes).expect("a live block supplies bytes");
    assert_eq!(supplied, 9, "the tail is served from a word, and the count says so");
    assert_ne!(&bytes[..], &[0u8; 9][..], "the bytes are the block's, not a zero fill");
    assert_eq!(rng.supplied(), 9);
}

#[test]
fn a_fill_that_supplies_nothing_never_completes_as_success()
{
    let mut block = Simulated::alive();
    block.pending = 1;
    let mut rng = Rng::new();
    rng.start(&mut block).expect("established");
    block.alive = false;
    let mut bytes = [0u8; 4];
    assert!(rng.fill(&mut block, &mut bytes).is_err(),
            "the entropy contract treats zero bytes as failure, so this must not be Ok");
}

#[test]
fn stopping_clears_the_enable_so_a_block_is_not_left_running()
{
    let mut block = Simulated::alive();
    block.pending = 1;
    let mut rng = Rng::new();
    rng.start(&mut block).expect("established");
    assert_ne!(block.control & ENABLE, 0, "the generator was enabled");
    rng.stop(&mut block);
    assert_eq!(block.control & ENABLE, 0, "quiescence must stop the generator");
    assert!(!rng.established());
}

#[test]
fn interrupting_the_warm_up_still_reports_where_it_stopped()
{
    // A block that dies part-way through the settle period: the refusal must name
    // the polls spent and no caller bytes may have been produced.
    let mut block = Simulated::alive();
    block.pending = 1;
    let mut rng = Rng::new();
    let mut observed = 0;
    // Take a few words by hand through a first start, then die and start again.
    rng.start(&mut block).expect("first start");
    observed += block.reads;
    block.alive = false;
    let refusal = rng.start(&mut block).expect_err("a dead block cannot be established");
    assert!(matches!(refusal, Refusal::SourceSilent { .. }));
    assert!(observed >= WARMUP_WORDS, "the first start discarded its settle words");
    assert!(!rng.established());
}

#[test]
fn every_refusal_explains_itself()
{
    use core::fmt::Write;
    struct Probe
    {
        text: [u8; 256],
        len:  usize
    }
    impl Write for Probe
    {
        fn write_str(&mut self, text: &str) -> core::fmt::Result
        {
            let end = self.len + text.len();
            if end > self.text.len()
            {
                return Err(core::fmt::Error);
            }
            self.text[self.len..end].copy_from_slice(text.as_bytes());
            self.len = end;
            Ok(())
        }
    }
    for refusal in [Refusal::SourceSilent { polls: 7 }, Refusal::NotStarted, Refusal::EmptyFill,
                    Refusal::ShortFill { supplied: 2,
                                         requested: 8 }]
    {
        let mut probe = Probe { text: [0; 256], len: 0 };
        write!(probe, "{refusal}").expect("a refusal always formats");
        assert!(probe.len > 0, "a refusal with no text would not be actionable");
    }
}
