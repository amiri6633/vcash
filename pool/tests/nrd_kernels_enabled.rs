// Copyright 2020 The Grin Developers
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

pub mod common;

use self::core::consensus;
use self::core::core::verifier_cache::LruVerifierCache;
use self::core::core::{HeaderVersion, KernelFeatures, NRDRelativeHeight};
use self::core::global;
use self::keychain::{ExtKeychain, Keychain};
use self::pool::types::PoolError;
use self::util::RwLock;
use crate::common::*;
use grin_core as core;
use grin_keychain as keychain;
use grin_pool as pool;
use grin_util as util;
use std::sync::Arc;

#[test]
fn test_nrd_kernels_enabled() {
	util::init_test_logger();
	global::set_local_chain_type(global::ChainTypes::AutomatedTesting);
	global::set_local_nrd_enabled(true);

	let keychain: ExtKeychain = Keychain::from_random_seed(false).unwrap();

	let db_root = "target/.nrd_kernels_enabled";
	clean_output_dir(db_root.into());

	let genesis = genesis_block(&keychain);
	let chain = Arc::new(init_chain(db_root, genesis));
	let verifier_cache = Arc::new(RwLock::new(LruVerifierCache::new()));

	// Initialize a new pool with our chain adapter.
	let mut pool = init_transaction_pool(
		Arc::new(ChainAdapter {
			chain: chain.clone(),
		}),
		verifier_cache,
	);

	// Add some blocks.
	add_some_blocks(&chain, 3, &keychain);

	// Spend the initial coinbase.
	let header_1 = chain.get_header_by_height(1).unwrap();
	let tx = test_transaction_spending_coinbase(&keychain, &header_1, vec![10, 20, 30, 40]);
	add_block(&chain, vec![tx], &keychain);

	let tx_1 = test_transaction_with_kernel_features(
		&keychain,
		vec![10, 20],
		vec![24],
		KernelFeatures::NoRecentDuplicate {
			fee: 6,
			relative_height: NRDRelativeHeight::new(144).unwrap(),
		},
	);

	let header = chain.head_header().unwrap();
	assert!(header.version < HeaderVersion(4));

	assert_eq!(
		pool.add_to_pool(test_source(), tx_1.clone(), false, &header),
		Err(PoolError::NRDKernelPreHF3)
	);

	// Now mine several more blocks out to HF3
	add_some_blocks(&chain, 5, &keychain);
	let header = chain.head_header().unwrap();
	assert_eq!(header.height, consensus::TESTING_THIRD_HARD_FORK);
	assert_eq!(header.version, HeaderVersion(2));

	// NRD kernel support not enabled via feature flag, so not valid.
	assert_eq!(
		pool.add_to_pool(test_source(), tx_1.clone(), false, &header),
		Ok(())
	);

	assert_eq!(pool.total_size(), 1);
	let txs = pool.prepare_mineable_transactions().unwrap();
	assert_eq!(txs.len(), 1);

	// Cleanup db directory
	clean_output_dir(db_root.into());
}
