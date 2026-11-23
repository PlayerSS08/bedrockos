//! Pipes: a bounded byte queue with a count of open read ends and write ends. Pure, host-tested.
//!
//! The rules are the Unix ones. A read from an empty pipe waits while a writer exists and reports end of
//! file once none does. A write to a pipe nobody can read is an error (`Broken`), and a write to a full
//! pipe waits. "Waits" is the caller's job: this module only answers `WouldBlock`.

use alloc::collections::VecDeque;

/// Bytes one pipe holds before writers must wait.
pub const CAPACITY: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadResult {
    Data(usize),
    WouldBlock,
    Eof,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteResult {
    Wrote(usize),
    WouldBlock,
    /// No read end is open any more.
    Broken,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End {
    Read,
    Write,
}

struct Pipe {
    buf: VecDeque<u8>,
    readers: u16,
    writers: u16,
}

pub struct PipeTable<const N: usize> {
    slots: [Option<Pipe>; N],
}

impl<const N: usize> Default for PipeTable<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> PipeTable<N> {
    pub const fn new() -> Self {
        Self { slots: [const { None }; N] }
    }

    /// A new pipe with one read end and one write end open. `None` when every slot is in use.
    pub fn create(&mut self) -> Option<usize> {
        let id = self.slots.iter().position(Option::is_none)?;
        self.slots[id] = Some(Pipe { buf: VecDeque::new(), readers: 1, writers: 1 });
        Some(id)
    }

    /// One more descriptor refers to this end (`dup2`, `fork`). False for a pipe that does not exist.
    pub fn retain(&mut self, id: usize, end: End) -> bool {
        match self.slots.get_mut(id).and_then(Option::as_mut) {
            Some(p) => {
                match end {
                    End::Read => p.readers = p.readers.saturating_add(1),
                    End::Write => p.writers = p.writers.saturating_add(1),
                }
                true
            }
            None => false,
        }
    }

    /// One descriptor of this end was closed. The pipe goes away with its last descriptor.
    pub fn release(&mut self, id: usize, end: End) {
        let Some(slot) = self.slots.get_mut(id) else { return };
        let Some(p) = slot.as_mut() else { return };
        match end {
            End::Read => p.readers = p.readers.saturating_sub(1),
            End::Write => p.writers = p.writers.saturating_sub(1),
        }
        if p.readers == 0 && p.writers == 0 {
            *slot = None;
        }
    }

    pub fn read(&mut self, id: usize, out: &mut [u8]) -> ReadResult {
        let Some(p) = self.slots.get_mut(id).and_then(Option::as_mut) else { return ReadResult::Eof };
        if out.is_empty() {
            return ReadResult::Data(0);
        }
        if p.buf.is_empty() {
            return if p.writers == 0 { ReadResult::Eof } else { ReadResult::WouldBlock };
        }
        let n = out.len().min(p.buf.len());
        for b in out.iter_mut().take(n) {
            *b = p.buf.pop_front().unwrap_or(0);
        }
        ReadResult::Data(n)
    }

    /// Writes as much of `data` as fits. A partial write is reported as its size, like `write(2)`.
    pub fn write(&mut self, id: usize, data: &[u8]) -> WriteResult {
        let Some(p) = self.slots.get_mut(id).and_then(Option::as_mut) else { return WriteResult::Broken };
        if p.readers == 0 {
            return WriteResult::Broken;
        }
        if data.is_empty() {
            return WriteResult::Wrote(0);
        }
        let room = CAPACITY - p.buf.len();
        if room == 0 {
            return WriteResult::WouldBlock;
        }
        let n = room.min(data.len());
        p.buf.extend(&data[..n]);
        WriteResult::Wrote(n)
    }

    pub fn in_use(&self) -> usize {
        self.slots.iter().flatten().count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_come_out_in_order() {
        let mut t = PipeTable::<4>::new();
        let p = t.create().unwrap();
        assert_eq!(t.write(p, b"hello "), WriteResult::Wrote(6));
        assert_eq!(t.write(p, b"world"), WriteResult::Wrote(5));
        let mut b = [0u8; 8];
        assert_eq!(t.read(p, &mut b), ReadResult::Data(8));
        assert_eq!(&b, b"hello wo");
        assert_eq!(t.read(p, &mut b), ReadResult::Data(3));
        assert_eq!(&b[..3], b"rld");
    }

    #[test]
    fn empty_pipe_waits_while_a_writer_exists_then_reports_eof() {
        let mut t = PipeTable::<4>::new();
        let p = t.create().unwrap();
        let mut b = [0u8; 4];
        assert_eq!(t.read(p, &mut b), ReadResult::WouldBlock);
        assert_eq!(t.write(p, b"x"), WriteResult::Wrote(1));
        t.release(p, End::Write);
        // What was written before the close is still delivered, then end of file.
        assert_eq!(t.read(p, &mut b), ReadResult::Data(1));
        assert_eq!(t.read(p, &mut b), ReadResult::Eof);
    }

    #[test]
    fn writing_with_no_reader_is_broken_and_a_full_pipe_waits() {
        let mut t = PipeTable::<4>::new();
        let p = t.create().unwrap();
        let big = vec![7u8; CAPACITY + 100];
        assert_eq!(t.write(p, &big), WriteResult::Wrote(CAPACITY));
        assert_eq!(t.write(p, b"more"), WriteResult::WouldBlock);
        let mut b = [0u8; 10];
        assert_eq!(t.read(p, &mut b), ReadResult::Data(10));
        assert_eq!(t.write(p, &big), WriteResult::Wrote(10));
        t.release(p, End::Read);
        assert_eq!(t.write(p, b"x"), WriteResult::Broken);
    }

    #[test]
    fn a_pipe_lives_until_its_last_end_closes() {
        let mut t = PipeTable::<2>::new();
        let a = t.create().unwrap();
        let b = t.create().unwrap();
        assert_eq!(t.create(), None);
        // fork: both ends gain a second holder.
        assert!(t.retain(a, End::Read));
        assert!(t.retain(a, End::Write));
        t.release(a, End::Read);
        t.release(a, End::Write);
        assert_eq!(t.in_use(), 2);
        t.release(a, End::Write);
        let mut buf = [0u8; 1];
        assert_eq!(t.read(a, &mut buf), ReadResult::Eof);
        t.release(a, End::Read);
        assert_eq!(t.in_use(), 1);
        // The freed slot is reused; the other pipe is untouched.
        assert_eq!(t.create(), Some(a));
        assert_eq!(t.write(b, b"k"), WriteResult::Wrote(1));
    }

    #[test]
    fn unknown_ids_and_extra_releases_do_no_harm() {
        let mut t = PipeTable::<2>::new();
        let mut buf = [0u8; 1];
        assert_eq!(t.read(5, &mut buf), ReadResult::Eof);
        assert_eq!(t.write(5, b"x"), WriteResult::Broken);
        assert!(!t.retain(1, End::Read));
        t.release(1, End::Read);
        let p = t.create().unwrap();
        t.release(p, End::Read);
        t.release(p, End::Read);
        t.release(p, End::Write);
        assert_eq!(t.in_use(), 0);
        assert_eq!(t.read(p, &mut []), ReadResult::Eof);
    }

    #[test]
    fn zero_length_calls_do_not_block() {
        let mut t = PipeTable::<2>::new();
        let p = t.create().unwrap();
        assert_eq!(t.read(p, &mut []), ReadResult::Data(0));
        assert_eq!(t.write(p, &[]), WriteResult::Wrote(0));
    }
}
