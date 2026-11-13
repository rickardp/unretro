// Bounds for allocations whose size comes from untrusted header fields.
// Which helpers are used depends on the enabled format features.
#![allow(dead_code)]

#[cfg(all(feature = "no_std", not(feature = "std")))]
use alloc::alloc::alloc_zeroed;
#[cfg(not(all(feature = "no_std", not(feature = "std"))))]
use std::alloc::alloc_zeroed;

use crate::MAX_DECOMPRESSED_SIZE;
use crate::compat::{Vec, format};
use crate::error::{Error, Result};

// Largest buffer reserved up front on the strength of a declared size alone.
// Anything beyond this grows only as real data is decoded.
pub(crate) const PREALLOC_LIMIT: usize = 1024 * 1024;

const fn saturate(value: u64, max: u64) -> u64 {
    if value > max { max } else { value }
}

// `MAX_DECOMPRESSED_SIZE` as a `usize`. A plain `as usize` cast truncates
// 4 GiB to 0 on 32-bit targets, which would reject every non-empty entry.
pub(crate) const fn max_decompressed_size() -> usize {
    saturate(MAX_DECOMPRESSED_SIZE, usize::MAX as u64) as usize
}

// Capacity to reserve for an output whose declared size is `declared`.
pub(crate) const fn prealloc_hint(declared: u64) -> usize {
    saturate(declared, PREALLOC_LIMIT as u64) as usize
}

// Validates a declared output size that has to be allocated in full.
pub(crate) fn checked_output_size(declared: u64, what: &str) -> Result<usize> {
    if declared > MAX_DECOMPRESSED_SIZE {
        return Err(Error::decompression(format!(
            "{what}: declared size {declared} exceeds {MAX_DECOMPRESSED_SIZE} byte limit"
        )));
    }
    usize::try_from(declared).map_err(|_| {
        Error::decompression(format!(
            "{what}: declared size {declared} is not addressable on this target"
        ))
    })
}

// Zero-filled buffer of exactly `len` bytes, failing instead of aborting
// when the allocator cannot provide it.
#[allow(unsafe_code)]
pub(crate) fn zeroed_buffer(len: usize, what: &str) -> Result<Vec<u8>> {
    let fail = || Error::decompression(format!("{what}: cannot allocate {len} bytes"));
    if len == 0 {
        return Ok(Vec::new());
    }
    let layout = core::alloc::Layout::array::<u8>(len).map_err(|_| fail())?;
    // `vec![0; len]` aborts on allocation failure. Probing with `try_reserve`
    // first is no substitute: macOS hands the freed probe block back and
    // zeroes it by hand, committing every page. Ask for zeroed pages directly.
    // SAFETY: `layout` has non-zero size. A non-null result is `len` zeroed
    // bytes from the global allocator with the layout `Vec<u8>` frees with.
    unsafe {
        let ptr = alloc_zeroed(layout);
        if ptr.is_null() {
            return Err(fail());
        }
        Ok(Vec::from_raw_parts(ptr, len, len))
    }
}

// Smallest cumulative budget, so tiny valid containers are never rejected.
pub(crate) const BUDGET_FLOOR: u64 = 16 * 1024 * 1024;

// Cumulative cap on bytes copied out of one container. Per-entry limits do
// not help when many entries alias the same input bytes.
pub(crate) struct OutputBudget {
    remaining: u64,
}

impl OutputBudget {
    // Allows `ratio` times the input size, at least `BUDGET_FLOOR` and at
    // most `MAX_DECOMPRESSED_SIZE`.
    pub(crate) fn for_input(input_len: usize, ratio: u64) -> Self {
        let scaled = (input_len as u64).saturating_mul(ratio);
        let floored = if scaled < BUDGET_FLOOR {
            BUDGET_FLOOR
        } else {
            scaled
        };
        Self {
            remaining: saturate(floored, MAX_DECOMPRESSED_SIZE),
        }
    }

    pub(crate) const fn remaining(&self) -> u64 {
        self.remaining
    }

    // Accounts for `bytes` of output, failing once the budget is spent.
    pub(crate) fn charge(&mut self, bytes: usize, what: &str) -> Result<()> {
        match self.remaining.checked_sub(bytes as u64) {
            Some(left) => {
                self.remaining = left;
                Ok(())
            }
            None => Err(Error::decompression(format!(
                "{what}: output exceeds the size budget for this input"
            ))),
        }
    }
}

// Smallest nesting budget: an entry of the largest size a container may yield
// (a sparse disk image, say) can be opened along with as much below it.
pub(crate) const NESTING_FLOOR: u64 = 2 * MAX_DECOMPRESSED_SIZE;

// Bytes allowed per input byte. Compression makes a legitimate inner
// container far larger than its outer one, so this is generous.
pub(crate) const NESTING_RATIO: u64 = 64;

// Charge for opening a nested container, so that empty and tiny ones are
// bounded in number as well.
pub(crate) const NESTING_OPEN_CHARGE: u64 = 4096;

// Cumulative cap on the bytes that the containers of one input yield, at all
// nesting levels together. Per-container budgets do not compose: a container
// whose entries alias its own bytes k times costs k^depth once every entry is
// opened again, and each level claims its own minimum allowance.
pub(crate) struct NestingBudget {
    remaining: u64,
    refused: bool,
}

impl NestingBudget {
    pub(crate) fn for_input(input_len: usize) -> Self {
        let scaled = (input_len as u64).saturating_mul(NESTING_RATIO);
        Self {
            remaining: if scaled < NESTING_FLOOR {
                NESTING_FLOOR
            } else {
                scaled
            },
            refused: false,
        }
    }

    // Accounts for an entry a container has yielded. The entry exists
    // already, so this only uses up what later `open` calls may draw on.
    pub(crate) const fn spend(&mut self, bytes: usize) {
        self.remaining = self.remaining.saturating_sub(bytes as u64);
    }

    // Whether another nested container may be opened. `unspent` is its size
    // when no container yielded it. A refusal is final.
    pub(crate) fn open(&mut self, unspent: usize) -> NestingCharge {
        let cost = NESTING_OPEN_CHARGE.saturating_add(unspent as u64);
        if let Some(left) = self.remaining.checked_sub(cost) {
            self.remaining = left;
            return NestingCharge::Accepted;
        }
        self.remaining = 0;
        if core::mem::replace(&mut self.refused, true) {
            NestingCharge::Refused
        } else {
            NestingCharge::FirstRefused
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NestingCharge {
    Accepted,
    // The first refusal, to be reported once.
    FirstRefused,
    Refused,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saturate_clamps_instead_of_truncating() {
        // 32-bit address space: 4 GiB must clamp to the maximum, not wrap to 0.
        let max_32 = u64::from(u32::MAX);
        assert_eq!(saturate(MAX_DECOMPRESSED_SIZE, max_32), max_32);
        assert_eq!(saturate(u64::MAX, max_32), max_32);
        assert_eq!(saturate(1234, max_32), 1234);
    }

    #[test]
    fn max_decompressed_size_is_never_zero() {
        let max = max_decompressed_size();
        assert_ne!(max, 0);
        assert_eq!(
            max as u64,
            MAX_DECOMPRESSED_SIZE.min(usize::MAX as u64),
            "limit must be the documented cap or the address-space maximum"
        );
    }

    #[test]
    fn prealloc_hint_passes_small_sizes_through() {
        assert_eq!(prealloc_hint(0), 0);
        assert_eq!(prealloc_hint(4096), 4096);
        assert_eq!(prealloc_hint(PREALLOC_LIMIT as u64), PREALLOC_LIMIT);
    }

    #[test]
    fn prealloc_hint_caps_declared_sizes() {
        assert_eq!(prealloc_hint(PREALLOC_LIMIT as u64 + 1), PREALLOC_LIMIT);
        assert_eq!(prealloc_hint(MAX_DECOMPRESSED_SIZE), PREALLOC_LIMIT);
        assert_eq!(prealloc_hint(u64::MAX), PREALLOC_LIMIT);
    }

    #[test]
    fn checked_output_size_accepts_sizes_within_limit() {
        assert_eq!(checked_output_size(0, "t").unwrap(), 0);
        assert_eq!(checked_output_size(1 << 20, "t").unwrap(), 1 << 20);
    }

    #[test]
    fn checked_output_size_rejects_sizes_over_limit() {
        assert!(checked_output_size(MAX_DECOMPRESSED_SIZE + 1, "t").is_err());
        let err = checked_output_size(u64::MAX, "UDIF").unwrap_err();
        assert!(err.to_string().contains("UDIF"), "{err}");
    }

    #[test]
    fn zeroed_buffer_returns_zeroed_bytes() {
        let buf = zeroed_buffer(4096, "t").unwrap();
        assert_eq!(buf.len(), 4096);
        assert!(buf.iter().all(|&b| b == 0));
        assert!(zeroed_buffer(0, "t").unwrap().is_empty());
    }

    #[test]
    fn zeroed_buffer_fails_cleanly_when_unallocatable() {
        // Larger than any allocation Rust permits (> isize::MAX).
        assert!(zeroed_buffer(usize::MAX, "t").is_err());
    }

    #[test]
    fn output_budget_enforces_floor_and_ratio() {
        let mut small = OutputBudget::for_input(10, 2);
        assert!(small.charge(BUDGET_FLOOR as usize, "t").is_ok());
        assert!(small.charge(1, "t").is_err());

        let input = BUDGET_FLOOR as usize;
        let mut scaled = OutputBudget::for_input(input, 2);
        assert!(scaled.charge(input, "t").is_ok());
        assert!(scaled.charge(input, "t").is_ok());
        let err = scaled.charge(1, "HFS").unwrap_err();
        assert!(err.to_string().contains("HFS"), "{err}");
    }

    #[test]
    fn output_budget_never_exceeds_global_cap() {
        let mut budget = OutputBudget::for_input(usize::MAX, u64::MAX);
        assert_eq!(budget.remaining, MAX_DECOMPRESSED_SIZE);
        assert!(budget.charge(0, "t").is_ok());
    }

    #[test]
    fn nesting_budget_refusal_is_final_and_flagged_once() {
        let mut budget = NestingBudget::for_input(10);
        assert_eq!(budget.remaining, NESTING_FLOOR);
        budget.remaining = 3 * NESTING_OPEN_CHARGE;
        assert_eq!(budget.open(0), NestingCharge::Accepted);
        budget.spend(NESTING_OPEN_CHARGE as usize);
        assert_eq!(budget.open(1), NestingCharge::FirstRefused);
        // Would have fit before the refusal.
        assert_eq!(budget.open(0), NestingCharge::Refused);
    }

    #[test]
    fn nesting_budget_spending_saturates() {
        let mut budget = NestingBudget::for_input(10);
        budget.spend(usize::MAX);
        budget.spend(1);
        assert_eq!(budget.open(0), NestingCharge::FirstRefused);
    }

    #[test]
    fn nesting_budget_scales_with_large_inputs() {
        let input = (NESTING_FLOOR / NESTING_RATIO) as usize * 2;
        assert_eq!(NestingBudget::for_input(input).remaining, 2 * NESTING_FLOOR);
        assert_eq!(
            NestingBudget::for_input(usize::MAX).remaining,
            (usize::MAX as u64).saturating_mul(NESTING_RATIO)
        );
    }
}
