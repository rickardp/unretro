//! RAR readers written only from the prose specifications in `docs/rar/`
//! (clean-room; see `docs/rar/README.md`).

pub(crate) mod container4;
pub(crate) mod container5;
#[cfg(test)]
mod extract;
pub(crate) mod filters;
pub(crate) mod ppmd;
pub(crate) mod rar15;
pub(crate) mod rar20;
pub(crate) mod rar29;
pub(crate) mod rar5;

// Why a RAR 1.5 or RAR 2.0 stream was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DecodeError {
    UnexpectedEof,
    InvalidHuffmanCode,
    // The stream contradicts itself or the declared size.
    IncompleteData,
    BufferOverflow,
    // `offset` reaches back past the `position` bytes produced so far.
    InvalidBackReference { offset: u32, position: u32 },
}

impl core::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UnexpectedEof => f.write_str("the packed data end early"),
            Self::InvalidHuffmanCode => f.write_str("invalid prefix code"),
            Self::IncompleteData => f.write_str("corrupt packed data"),
            Self::BufferOverflow => f.write_str("more data than the declared size"),
            Self::InvalidBackReference { offset, position } => write!(
                f,
                "match distance {offset} reaches before the start ({position} bytes produced)"
            ),
        }
    }
}
