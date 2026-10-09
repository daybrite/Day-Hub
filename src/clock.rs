//! Native clocks on desktop/mobile; Day's host clock on raw WebAssembly, where
//! `std::time::{Instant, SystemTime}::now()` panic and wasm-bindgen is unavailable.

#[cfg(not(target_arch = "wasm32"))]
pub use std::time::Instant;

pub fn unix_seconds() -> u64 {
    #[cfg(target_arch = "wasm32")]
    {
        day_dom::now_epoch_ms() / 1000
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }
}

#[cfg(target_arch = "wasm32")]
pub use browser::Instant;

#[cfg(target_arch = "wasm32")]
mod browser {
    use std::{cell::Cell, time::Duration};

    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
    pub struct Instant(Duration);

    impl Instant {
        pub fn now() -> Self {
            // Day exposes Date.now() through its raw Wasm host. Clamp backwards
            // clock adjustments so elapsed durations never underflow.
            thread_local! { static LAST: Cell<u64> = const { Cell::new(0) }; }
            let millis = LAST.with(|last| {
                let now = day_dom::now_epoch_ms().max(last.get());
                last.set(now);
                now
            });
            Self(Duration::from_millis(millis))
        }

        pub fn elapsed(self) -> Duration {
            Self::now().saturating_duration_since(self)
        }

        pub fn saturating_duration_since(self, earlier: Self) -> Duration {
            self.0.saturating_sub(earlier.0)
        }
    }

    impl std::ops::Add<Duration> for Instant {
        type Output = Self;

        fn add(self, duration: Duration) -> Self {
            Self(self.0.saturating_add(duration))
        }
    }
}
