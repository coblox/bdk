// Magical Bitcoin Library
// Written in 2020 by
//     Alekos Filini <alekos.filini@gmail.com>
//
// Copyright (c) 2020 Magical Bitcoin
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.

use bitcoin::secp256k1::{All, Secp256k1};
use bitcoin::util::bip32;

use miniscript::descriptor::DescriptorPublicKeyCtx;
use miniscript::{MiniscriptKey, Satisfier, ToPublicKey};

// De-facto standard "dust limit" (even though it should change based on the output type)
pub const DUST_LIMIT_SATOSHI: u64 = 546;

// MSB of the nSequence. If set there's no consensus-constraint, so it must be disabled when
// spending using CSV in order to enforce CSV rules
pub(crate) const SEQUENCE_LOCKTIME_DISABLE_FLAG: u32 = 1 << 31;
// When nSequence is lower than this flag the timelock is interpreted as block-height-based,
// otherwise it's time-based
pub(crate) const SEQUENCE_LOCKTIME_TYPE_FLAG: u32 = 1 << 22;
// Mask for the bits used to express the timelock
pub(crate) const SEQUENCE_LOCKTIME_MASK: u32 = 0x0000FFFF;

// Threshold for nLockTime to be considered a block-height-based timelock rather than time-based
pub(crate) const BLOCKS_TIMELOCK_THRESHOLD: u32 = 500000000;

/// Trait to check if a value is below the dust limit
// we implement this trait to make sure we don't mess up the comparison with off-by-one like a <
// instead of a <= etc. The constant value for the dust limit is not public on purpose, to
// encourage the usage of this trait.
pub trait IsDust {
    /// Check whether or not a value is below dust limit
    fn is_dust(&self) -> bool;
}

impl IsDust for u64 {
    fn is_dust(&self) -> bool {
        *self <= DUST_LIMIT_SATOSHI
    }
}

pub struct After {
    pub current_height: Option<u32>,
    pub assume_height_reached: bool,
}

impl After {
    pub(crate) fn new(current_height: Option<u32>, assume_height_reached: bool) -> After {
        After {
            current_height,
            assume_height_reached,
        }
    }
}

pub(crate) fn check_nsequence_rbf(rbf: u32, csv: u32) -> bool {
    // This flag cannot be set in the nSequence when spending using OP_CSV
    if rbf & SEQUENCE_LOCKTIME_DISABLE_FLAG != 0 {
        return false;
    }

    let mask = SEQUENCE_LOCKTIME_TYPE_FLAG | SEQUENCE_LOCKTIME_MASK;
    let rbf = rbf & mask;
    let csv = csv & mask;

    // Both values should be represented in the same unit (either time-based or
    // block-height based)
    if (rbf < SEQUENCE_LOCKTIME_TYPE_FLAG) != (csv < SEQUENCE_LOCKTIME_TYPE_FLAG) {
        return false;
    }

    // The value should be at least `csv`
    if rbf < csv {
        return false;
    }

    true
}

pub(crate) fn check_nlocktime(nlocktime: u32, required: u32) -> bool {
    // Both values should be expressed in the same unit
    if (nlocktime < BLOCKS_TIMELOCK_THRESHOLD) != (required < BLOCKS_TIMELOCK_THRESHOLD) {
        return false;
    }

    // The value should be at least `required`
    if nlocktime < required {
        return false;
    }

    true
}

impl<ToPkCtx: Copy, Pk: MiniscriptKey + ToPublicKey<ToPkCtx>> Satisfier<ToPkCtx, Pk> for After {
    fn check_after(&self, n: u32) -> bool {
        if let Some(current_height) = self.current_height {
            current_height >= n
        } else {
            self.assume_height_reached
        }
    }
}

pub struct Older {
    pub current_height: Option<u32>,
    pub create_height: Option<u32>,
    pub assume_height_reached: bool,
}

impl Older {
    pub(crate) fn new(
        current_height: Option<u32>,
        create_height: Option<u32>,
        assume_height_reached: bool,
    ) -> Older {
        Older {
            current_height,
            create_height,
            assume_height_reached,
        }
    }
}

impl<ToPkCtx: Copy, Pk: MiniscriptKey + ToPublicKey<ToPkCtx>> Satisfier<ToPkCtx, Pk> for Older {
    fn check_older(&self, n: u32) -> bool {
        if let Some(current_height) = self.current_height {
            // TODO: test >= / >
            current_height as u64 >= self.create_height.unwrap_or(0) as u64 + n as u64
        } else {
            self.assume_height_reached
        }
    }
}

pub(crate) type SecpCtx = Secp256k1<All>;
pub(crate) fn descriptor_to_pk_ctx(secp: &SecpCtx) -> DescriptorPublicKeyCtx<'_, All> {
    // Create a `to_pk_ctx` with a dummy derivation index, since we always use this on descriptor
    // that have already been derived with `Descriptor::derive()`, so the child number added here
    // is ignored.
    DescriptorPublicKeyCtx::new(secp, bip32::ChildNumber::Normal { index: 0 })
}

pub struct ChunksIterator<I: Iterator> {
    iter: I,
    size: usize,
}

impl<I: Iterator> ChunksIterator<I> {
    #[allow(dead_code)]
    pub fn new(iter: I, size: usize) -> Self {
        ChunksIterator { iter, size }
    }
}

impl<I: Iterator> Iterator for ChunksIterator<I> {
    type Item = Vec<<I as std::iter::Iterator>::Item>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut v = Vec::new();
        for _ in 0..self.size {
            let e = self.iter.next();

            match e {
                None => break,
                Some(val) => v.push(val),
            }
        }

        if v.is_empty() {
            return None;
        }

        Some(v)
    }
}

#[cfg(test)]
mod test {
    use super::{
        check_nlocktime, check_nsequence_rbf, BLOCKS_TIMELOCK_THRESHOLD,
        SEQUENCE_LOCKTIME_TYPE_FLAG,
    };
    use crate::types::FeeRate;

    #[test]
    fn test_fee_from_btc_per_kb() {
        let fee = FeeRate::from_btc_per_kvb(1e-5);
        assert!((fee.as_sat_vb() - 1.0).abs() < 0.0001);
    }

    #[test]
    fn test_fee_from_sats_vbyte() {
        let fee = FeeRate::from_sat_per_vb(1.0);
        assert!((fee.as_sat_vb() - 1.0).abs() < 0.0001);
    }

    #[test]
    fn test_fee_default_min_relay_fee() {
        let fee = FeeRate::default_min_relay_fee();
        assert!((fee.as_sat_vb() - 1.0).abs() < 0.0001);
    }

    #[test]
    fn test_check_nsequence_rbf_msb_set() {
        let result = check_nsequence_rbf(0x80000000, 5000);
        assert_eq!(result, false);
    }

    #[test]
    fn test_check_nsequence_rbf_lt_csv() {
        let result = check_nsequence_rbf(4000, 5000);
        assert_eq!(result, false);
    }

    #[test]
    fn test_check_nsequence_rbf_different_unit() {
        let result = check_nsequence_rbf(SEQUENCE_LOCKTIME_TYPE_FLAG + 5000, 5000);
        assert_eq!(result, false);
    }

    #[test]
    fn test_check_nsequence_rbf_mask() {
        let result = check_nsequence_rbf(0x3f + 10_000, 5000);
        assert_eq!(result, true);
    }

    #[test]
    fn test_check_nsequence_rbf_same_unit_blocks() {
        let result = check_nsequence_rbf(10_000, 5000);
        assert_eq!(result, true);
    }

    #[test]
    fn test_check_nsequence_rbf_same_unit_time() {
        let result = check_nsequence_rbf(
            SEQUENCE_LOCKTIME_TYPE_FLAG + 10_000,
            SEQUENCE_LOCKTIME_TYPE_FLAG + 5000,
        );
        assert_eq!(result, true);
    }

    #[test]
    fn test_check_nlocktime_lt_cltv() {
        let result = check_nlocktime(4000, 5000);
        assert_eq!(result, false);
    }

    #[test]
    fn test_check_nlocktime_different_unit() {
        let result = check_nlocktime(BLOCKS_TIMELOCK_THRESHOLD + 5000, 5000);
        assert_eq!(result, false);
    }

    #[test]
    fn test_check_nlocktime_same_unit_blocks() {
        let result = check_nlocktime(10_000, 5000);
        assert_eq!(result, true);
    }

    #[test]
    fn test_check_nlocktime_same_unit_time() {
        let result = check_nlocktime(
            BLOCKS_TIMELOCK_THRESHOLD + 10_000,
            BLOCKS_TIMELOCK_THRESHOLD + 5000,
        );
        assert_eq!(result, true);
    }
}
