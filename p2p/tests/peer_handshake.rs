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

use grin_core as core;
use grin_p2p as p2p;

use grin_util as util;
use grin_util::StopState;

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::{thread, time};

use crate::core::core::hash::Hash;
use crate::core::core::verifier_cache::LruVerifierCache;
use crate::core::global;
use crate::core::pow;
use crate::core::pow::Difficulty;
use crate::p2p::types::PeerAddr;
use crate::p2p::Peer;
use chain::types::NoopAdapter;
use grin_chain as chain;
use util::RwLock;

fn open_port() -> u16 {
	// use port 0 to allow the OS to assign an open port
	// TcpListener's Drop impl will unbind the port as soon as
	// listener goes out of scope
	let listener = TcpListener::bind("127.0.0.1:0").unwrap();
	listener.local_addr().unwrap().port()
}

// Setup test with AutomatedTesting chain_type;
fn test_setup() {
	global::set_local_chain_type(global::ChainTypes::AutomatedTesting);
	util::init_test_logger();
}

// Starts a server and connects a client peer to it to check handshake,
// followed by a ping/pong exchange to make sure the connection is live.
#[test]
fn peer_handshake() {
	test_setup();

	let p2p_config = p2p::P2PConfig {
		host: "127.0.0.1".parse().unwrap(),
		port: open_port(),
		peers_allow: None,
		peers_deny: None,
		..p2p::P2PConfig::default()
	};
	let net_adapter = Arc::new(p2p::DummyAdapter {});
	let verifier_cache = Arc::new(RwLock::new(LruVerifierCache::new()));
	let genesis_block = pow::mine_genesis_block().unwrap();

	let chain = chain::Chain::init(
		"test".to_string(),
		Arc::new(NoopAdapter {}),
		genesis_block,
		pow::verify_size,
		verifier_cache,
		false,
	)
	.unwrap();

	let server = Arc::new(
		p2p::Server::new(
			".grin",
			p2p::Capabilities::UNKNOWN,
			p2p_config.clone(),
			net_adapter.clone(),
			Hash::from_vec(&vec![]),
			Arc::new(StopState::new()),
			Arc::new(chain),
		)
		.unwrap(),
	);

	let p2p_inner = server.clone();
	let _ = thread::spawn(move || {
		// Test setup relies on thread local for chain_type so make sure we setup here.
		test_setup();
		p2p_inner.listen()
	});

	thread::sleep(time::Duration::from_secs(1));

	let addr = SocketAddr::new(p2p_config.host, p2p_config.port);
	let socket = TcpStream::connect_timeout(&addr, time::Duration::from_secs(10)).unwrap();

	let my_addr = PeerAddr("127.0.0.1:5000".parse().unwrap());
	let peer = Peer::connect(
		socket,
		p2p::Capabilities::UNKNOWN,
		Difficulty::min(),
		my_addr,
		&p2p::handshake::Handshake::new(Hash::from_vec(&vec![]), p2p_config.clone()),
		net_adapter,
	)
	.unwrap();

	assert!(peer.info.user_agent.ends_with(env!("CARGO_PKG_VERSION")));

	thread::sleep(time::Duration::from_secs(1));

	peer.send_ping(Difficulty::min(), 0).unwrap();
	thread::sleep(time::Duration::from_secs(1));

	let server_peer = server.peers.get_connected_peer(my_addr).unwrap();
	assert_eq!(server_peer.info.total_difficulty(), Difficulty::min());
	assert!(server.peers.peer_count() > 0);
}
