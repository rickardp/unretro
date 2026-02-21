//! Small compatibility layer for `std`/`no_std` builds.
//!
//! This keeps shared parsing code readable while allowing graceful
//! degradation of std-dependent backends.

#[cfg(all(feature = "no_std", not(feature = "std")))]
#[allow(unused_imports)]
pub use alloc::{
    borrow::ToOwned,
    boxed::Box,
    string::{String, ToString},
    vec::Vec,
};

#[cfg(not(all(feature = "no_std", not(feature = "std"))))]
#[allow(unused_imports)]
pub use std::{
    borrow::ToOwned,
    boxed::Box,
    string::{String, ToString},
    vec::Vec,
};

#[cfg(all(feature = "no_std", not(feature = "std")))]
#[allow(unused_imports)]
pub use alloc::{format, vec};

#[cfg(not(all(feature = "no_std", not(feature = "std"))))]
#[allow(unused_imports)]
pub use std::{format, vec};

#[cfg(not(all(feature = "no_std", not(feature = "std"))))]
#[allow(dead_code)]
pub type FastMap<K, V> = std::collections::HashMap<K, V>;

#[cfg(all(feature = "no_std", not(feature = "std")))]
#[allow(dead_code)]
pub type FastMap<K, V> = alloc::collections::BTreeMap<K, V>;
