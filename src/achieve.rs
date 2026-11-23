//! Achievements, Minecraft style: doing something for the first time ("open your first app", "scan for
//! Wi-Fi") pops up an "Achievement get!" toast on the desktop. `achievements` in ChatBox lists them.
//! Lock-free (two bit sets), so the shell, the desktop and drivers can all call [`unlock`].

use core::sync::atomic::{AtomicU32, Ordering::Relaxed};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Id {
    Welcome,
    FirstApp,
    Surfer,
    WifiScan,
    Online,
    Calculator,
    Writer,
    Snake,
    BigWindow,
    Neofetch,
    Explorer,
    TimeTraveller,
}

/// Title and how to get it, in `Id` order.
pub const LIST: [(&str, &str); 12] = [
    ("Welcome to the Overworld", "Sign in to BedrockOS"),
    ("Getting Wood", "Open your first app from the dock"),
    ("Crafting Table", "Load a web page in the Crafting Table browser"),
    ("Signal Found", "Scan for Wi-Fi networks (wifi scan)"),
    ("Hello, World", "Get an answer from the internet (ping)"),
    ("Redstone Engineer", "Work something out in the Calculator"),
    ("Book and Quill", "Write a note in Notes"),
    ("Snake Charmer", "Eat 5 apples in Snake"),
    ("Bigger is Better", "Maximise a window"),
    ("Who Am I?", "Run neofetch in ChatBox"),
    ("Explorer", "Open the Files app"),
    ("Time Traveller", "Open the Clock app"),
];

static UNLOCKED: AtomicU32 = AtomicU32::new(0);
/// Unlocked but not yet shown as a toast.
static PENDING: AtomicU32 = AtomicU32::new(0);

/// Marks `id` as done. The first time, it is queued for a toast and `true` is returned.
pub fn unlock(id: Id) -> bool {
    let bit = 1u32 << id as u8;
    if UNLOCKED.fetch_or(bit, Relaxed) & bit != 0 {
        return false;
    }
    PENDING.fetch_or(bit, Relaxed);
    true
}

pub fn is_unlocked(i: usize) -> bool {
    i < LIST.len() && UNLOCKED.load(Relaxed) & (1 << i) != 0
}

pub fn count() -> usize {
    UNLOCKED.load(Relaxed).count_ones() as usize
}

/// The next achievement waiting for its toast (oldest id first).
pub fn next_toast() -> Option<&'static str> {
    loop {
        let p = PENDING.load(Relaxed);
        if p == 0 {
            return None;
        }
        let i = p.trailing_zeros();
        if PENDING.compare_exchange(p, p & !(1 << i), Relaxed, Relaxed).is_ok() {
            return LIST.get(i as usize).map(|e| e.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlocks_once_and_queues_toasts() {
        // One test owns the globals (tests run in parallel).
        assert!(unlock(Id::Neofetch));
        assert!(!unlock(Id::Neofetch), "only the first time counts");
        assert!(unlock(Id::Welcome));
        assert!(is_unlocked(Id::Neofetch as usize) && is_unlocked(0));
        assert!(!is_unlocked(Id::Snake as usize));
        assert_eq!(count(), 2);
        assert_eq!(next_toast(), Some("Welcome to the Overworld"), "lowest id first");
        assert_eq!(next_toast(), Some("Who Am I?"));
        assert_eq!(next_toast(), None);
        assert_eq!(LIST.len(), Id::TimeTraveller as usize + 1);
    }
}
