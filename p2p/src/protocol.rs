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

use crate::chain;
use crate::conn::MessageHandler;
use crate::core::core::{hash::Hashed, CompactBlock};

use crate::core::global;
use crate::msg::{Consumed, Headers, Message, Msg, PeerAddrs, Pong, TxHashSetArchive, Type};
use crate::types::{AttachmentMeta, Error, NetAdapter, PeerInfo};
use chrono::prelude::Utc;
use rand::{thread_rng, Rng};
use std::fs::{self, File};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub struct Protocol {
	adapter: Arc<dyn NetAdapter>,
	peer_info: PeerInfo,
	state_sync_requested: Arc<AtomicBool>,
}

impl Protocol {
	pub fn new(
		adapter: Arc<dyn NetAdapter>,
		peer_info: PeerInfo,
		state_sync_requested: Arc<AtomicBool>,
	) -> Protocol {
		Protocol {
			adapter,
			peer_info,
			state_sync_requested,
		}
	}

	fn check_protocol_version(&self) -> Result<(), Error> {
		let minimum_compatible_protocol_version = if global::is_testnet() { 3 } else { 2 };
		//if self.adapter.total_height()? >= global::support_token_height() {
		if self.peer_info.version.value() < minimum_compatible_protocol_version {
			return Err(Error::LowProtocolVersion);
		}
		//}

		if self.adapter.total_height()? >= global::third_hard_fork_height() {
			if self.peer_info.version.value() < 3 {
				return Err(Error::LowProtocolVersion);
			}
		}

		Ok(())
	}
}

impl MessageHandler for Protocol {
	fn consume(&self, message: Message) -> Result<Consumed, Error> {
		let adapter = &self.adapter;

		// If we received a msg from a banned peer then log and drop it.
		// If we are getting a lot of these then maybe we are not cleaning
		// banned peers up correctly?
		if adapter.is_banned(self.peer_info.addr) {
			debug!(
				"handler: consume: peer {:?} banned, received: {}, dropping.",
				self.peer_info.addr, message,
			);
			return Ok(Consumed::Disconnect);
		}

		self.check_protocol_version()?;

		let consumed = match message {
			Message::Attachment(update, _) => {
				self.adapter.txhashset_download_update(
					update.meta.start_time,
					(update.meta.size - update.left) as u64,
					update.meta.size as u64,
				);

				if update.left == 0 {
					let meta = update.meta;
					trace!(
						"handle_payload: txhashset archive save to file {:?} success",
						meta.path,
					);

					let zip = File::open(meta.path.clone())?;
					let res =
						self.adapter
							.txhashset_write(meta.hash.clone(), zip, &self.peer_info)?;

					debug!(
						"handle_payload: txhashset archive for {} at {}, DONE. Data Ok: {}",
						meta.hash, meta.height, !res
					);

					if let Err(e) = fs::remove_file(meta.path.clone()) {
						warn!("fail to remove tmp file: {:?}. err: {}", meta.path, e);
					}
				}

				Consumed::None
			}

			Message::Ping(ping) => {
				adapter.peer_difficulty(self.peer_info.addr, ping.total_difficulty, ping.height);
				Consumed::Response(Msg::new(
					Type::Pong,
					Pong {
						total_difficulty: adapter.total_difficulty()?,
						height: adapter.total_height()?,
					},
					self.peer_info.version,
				)?)
			}

			Message::Pong(pong) => {
				adapter.peer_difficulty(self.peer_info.addr, pong.total_difficulty, pong.height);
				Consumed::None
			}

			Message::BanReason(ban_reason) => {
				error!("handle_payload: BanReason {:?}", ban_reason);
				Consumed::Disconnect
			}

			Message::TransactionKernel(h) => {
				debug!("handle_payload: received tx kernel: {}", h);
				adapter.tx_kernel_received(h, &self.peer_info)?;
				Consumed::None
			}

			Message::GetTransaction(h) => {
				debug!("handle_payload: GetTransaction: {}", h);
				let tx = adapter.get_transaction(h);
				if let Some(tx) = tx {
					Consumed::Response(Msg::new(Type::Transaction, tx, self.peer_info.version)?)
				} else {
					Consumed::None
				}
			}

			Message::Transaction(tx) => {
				debug!("handle_payload: received tx");
				adapter.transaction_received(tx, false)?;
				Consumed::None
			}

			Message::StemTransaction(tx) => {
				debug!("handle_payload: received stem tx");
				adapter.transaction_received(tx, true)?;
				Consumed::None
			}

			Message::GetBlock(h) => {
				trace!("handle_payload: GetBlock: {}", h);
				let bo = adapter.get_block(h, &self.peer_info);
				if let Some(b) = bo {
					Consumed::Response(Msg::new(Type::Block, b, self.peer_info.version)?)
				} else {
					Consumed::None
				}
			}

			Message::Block(b) => {
				debug!("handle_payload: received block");
				// We default to NONE opts here as we do not know know yet why this block was
				// received.
				// If we requested this block from a peer due to our node syncing then
				// the peer adapter will override opts to reflect this.
				adapter.block_received(b.into(), &self.peer_info, chain::Options::NONE)?;
				Consumed::None
			}

			Message::GetCompactBlock(h) => {
				if let Some(b) = adapter.get_block(h, &self.peer_info) {
					let cb: CompactBlock = b.into();
					Consumed::Response(Msg::new(Type::CompactBlock, cb, self.peer_info.version)?)
				} else {
					Consumed::None
				}
			}

			Message::CompactBlock(b) => {
				debug!("handle_payload: received compact block");
				adapter.compact_block_received(b.into(), &self.peer_info)?;
				Consumed::None
			}

			Message::GetHeaders(loc) => {
				// load headers from the locator
				let headers = adapter.locate_headers(&loc.hashes)?;

				// serialize and send all the headers over
				Consumed::Response(Msg::new(
					Type::Headers,
					Headers { headers },
					self.peer_info.version,
				)?)
			}

			// "header first" block propagation - if we have not yet seen this block
			// we can go request it from some of our peers
			Message::Header(header) => {
				adapter.header_received(header.into(), &self.peer_info)?;
				Consumed::None
			}

			Message::Headers(headers) => {
				adapter.headers_received(&headers, &self.peer_info)?;
				Consumed::None
			}

			Message::GetPeerAddrs(get_peers) => {
				let peers = adapter.find_peer_addrs(get_peers.capabilities);
				Consumed::Response(Msg::new(
					Type::PeerAddrs,
					PeerAddrs { peers },
					self.peer_info.version,
				)?)
			}

			Message::PeerAddrs(peer_addrs) => {
				adapter.peer_addrs_received(peer_addrs.peers);
				Consumed::None
			}

			Message::TxHashSetRequest(sm_req) => {
				debug!(
					"handle_payload: txhashset req for {} at {}",
					sm_req.hash, sm_req.height
				);

				let txhashset_header = self.adapter.txhashset_archive_header()?;
				let txhashset_header_hash = txhashset_header.hash();
				let txhashset = self.adapter.txhashset_read(txhashset_header_hash);

				if let Some(txhashset) = txhashset {
					let file_sz = txhashset.reader.metadata()?.len();
					let mut resp = Msg::new(
						Type::TxHashSetArchive,
						&TxHashSetArchive {
							height: txhashset_header.height as u64,
							hash: txhashset_header_hash,
							bytes: file_sz,
						},
						self.peer_info.version,
					)?;
					resp.add_attachment(txhashset.reader);
					Consumed::Response(resp)
				} else {
					Consumed::None
				}
			}

			Message::TxHashSetArchive(sm_arch) => {
				debug!(
					"handle_payload: txhashset archive for {} at {}. size={}",
					sm_arch.hash, sm_arch.height, sm_arch.bytes,
				);
				if !self.adapter.txhashset_receive_ready() {
					error!(
						"handle_payload: txhashset archive received but SyncStatus not on TxHashsetDownload",
					);
					return Err(Error::BadMessage);
				}
				if !self.state_sync_requested.load(Ordering::Relaxed) {
					error!("handle_payload: txhashset archive received but from the wrong peer",);
					return Err(Error::BadMessage);
				}
				// Update the sync state requested status
				self.state_sync_requested.store(false, Ordering::Relaxed);

				let start_time = Utc::now();
				self.adapter
					.txhashset_download_update(start_time, 0, sm_arch.bytes);

				let nonce: u32 = thread_rng().gen_range(0, 1_000_000);
				let path = self.adapter.get_tmpfile_pathname(format!(
					"txhashset-{}-{}.zip",
					start_time.timestamp(),
					nonce
				));

				let file = fs::OpenOptions::new()
					.write(true)
					.create_new(true)
					.open(path.clone())?;

				let meta = AttachmentMeta {
					size: sm_arch.bytes as usize,
					hash: sm_arch.hash,
					height: sm_arch.height,
					start_time,
					path,
				};

				Consumed::Attachment(Arc::new(meta), file)
			}
			Message::Unknown(_) => Consumed::None,
		};
		Ok(consumed)
	}
}
