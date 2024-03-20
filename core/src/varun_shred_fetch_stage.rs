//! The `varun_shred_fetch_stage` pulls shreds from UDP sockets and sends it to a channel.

use {
    crate::repair::serve_repair::ServeRepair,
    bytes::Bytes,
    crossbeam_channel::{unbounded, Receiver, RecvTimeoutError, Sender},
    itertools::Itertools,
    solana_ledger::shred::layout,
    solana_gossip::cluster_info::ClusterInfo,
    solana_ledger::shred::{should_discard_shred, ShredFetchStats},
    solana_perf::packet::{PacketBatch, PacketBatchRecycler, PacketFlags, PACKETS_PER_BATCH},
    solana_runtime::bank_forks::BankForks,
    solana_sdk::{
        clock::{Slot, DEFAULT_MS_PER_SLOT},
        epoch_schedule::EpochSchedule,
        feature_set::{self, FeatureSet},
        packet::{Meta, PACKET_DATA_SIZE},
        pubkey::Pubkey,
    },
    solana_streamer::streamer::{self, PacketBatchReceiver, StreamerReceiveStats},
    std::{
        net::{SocketAddr, UdpSocket},
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc, RwLock,
        },
        thread::{self, Builder, JoinHandle},
        time::{Duration, Instant},
    },
};
use solana_entry::entry::Entry;
use solana_ledger::{blockstore, shred};
use solana_ledger::blockstore::{BlockstoreError, MAX_DATA_SHREDS_PER_SLOT};
use solana_ledger::shred::{ReedSolomonCache, shred_code, Shredder};
// use solana_ledger::shred::{layout, shred_code, ShredType};
use solana_sdk::packet::Packet;
use solana_sdk::transaction::VersionedTransaction;
use crate::banking_stage::immutable_deserialized_packet::ImmutableDeserializedPacket;
use crate::repair::serve_repair::ShredRepairType::Shred;

const PACKET_COALESCE_DURATION: Duration = Duration::from_millis(1);

pub(crate) struct VarunShredFetchStage {
    thread_hdls: Vec<JoinHandle<()>>,
}

impl VarunShredFetchStage {
    // updates packets received on a channel and sends them on another channel
    fn modify_packets(
        recvr: PacketBatchReceiver,
        shred_version: u16,
        name: &'static str,
        flags: PacketFlags,
    ) {
        const STATS_SUBMIT_CADENCE: Duration = Duration::from_secs(1);
        let mut last_updated = Instant::now();
        let mut stats = ShredFetchStats::default();

        for mut packet_batch in recvr {
            // println!("packetreceived1");
            if last_updated.elapsed().as_millis() as u64 > DEFAULT_MS_PER_SLOT {
                last_updated = Instant::now();
                stats.shred_count += packet_batch.len();

                // Limit shreds to 2 epochs away.
                // let should_drop_legacy_shreds =
                //     |shred_slot| should_drop_legacy_shreds(shred_slot, &feature_set, &epoch_schedule);
                // let turbine_disabled = turbine_disabled.load(Ordering::Relaxed);
                let shreds: Vec<_> = packet_batch
                    .iter()
                    .filter(|p| !p.meta().discard())
                    .filter_map(shred::layout::get_shred)
                    .map(<[u8]>::to_vec)
                    .filter_map(|s| shred::Shred::new_from_serialized_shred(s).ok())
                    .collect();

                println!("=============Batch Started====={}=====", shreds.len());
                for (i, shred) in shreds.iter().enumerate() {

                    println!("index:{}", i);
                    // ShredCode(ShredCode)()
                    println!("{:#?}", &shred.common_header() );
                    println!("{:#?}", &shred.extract_specific_header() );
                }

                let deshred_payload_base = Shredder::deshred(&shreds).map_err(|e| {
                    BlockstoreError::InvalidShredData(Box::new(bincode::ErrorKind::Custom(format!(
                        "Could not reconstruct data block from constituent shreds, error: {e:?}"
                    ))))
                });

                if deshred_payload_base.is_ok() {
                    let deshred_payload = deshred_payload_base.unwrap();
                    debug!("{:?} shreds in last FEC set", shreds.len(),);
                    bincode::deserialize::<Vec<Entry>>(&deshred_payload).map_err(|e| {
                        BlockstoreError::InvalidShredData(Box::new(bincode::ErrorKind::Custom(format!(
                            "could not reconstruct entries: {e:?}"
                        ))))
                    });
                }

                ///////////////

                let mypacketbatch = packet_batch.clone();
                let desr_packets: Vec<_> = mypacketbatch.iter()
                    .filter_map(|p| {
                        let new_pack = p.clone();
                        let despacket = ImmutableDeserializedPacket::new(new_pack);
                        return despacket.ok()
                    })
                    .collect();

               if desr_packets.len() > 0 {
                   println!("found desr packet")
               };
                println!("packets");
                println!("{:#?}", desr_packets);

                // TODO - Might need this code to recover
                // let reed_solomon_cache = ReedSolomonCache::default();
                // // Test recovery
                // for (fec_data_shreds, fec_coding_shreds) in fec_data.values().zip(fec_coding.values()) {
                //     let first_data_index = fec_data_shreds.first().unwrap().index() as usize;
                //     let all_shreds: Vec<solana_ledger::shred::Shred> = fec_data_shreds
                //         .iter()
                //         .step_by(2)
                //         .chain(fec_coding_shreds.iter().step_by(2))
                //         .cloned()
                //         .collect();
                //     let recovered_data = Shredder::try_recovery(all_shreds, &reed_solomon_cache).unwrap();
                //     // Necessary in order to ensure the last shred in the slot
                //     // is part of the recovered set, and that the below `index`
                //     // calculation in the loop is correct


                for packet in packet_batch.iter_mut().filter(|p| !p.meta().discard()) {
                    // println!("packetreceived2");
                    if varun_should_discard_shred(packet) {
                        // println!("discarding packet")
                    }

                    // if turbine_disabled
                    //     || should_discard_shred(
                    //     packet,
                    //
                    //     shred_version,
                    //     should_drop_legacy_shreds,
                    //     &mut stats,
                    // )
                    // {
                    //     packet.meta_mut().set_discard(true);
                    // } else {
                    //     packet.meta_mut().flags.insert(flags);
                    // }
                }
                // stats.maybe_submit(name, STATS_SUBMIT_CADENCE);
                // if sendr.send(packet_batch).is_err() {
                //     break;
                // }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn packet_modifier(
        sockets: Vec<Arc<UdpSocket>>,
        exit: Arc<AtomicBool>,
        recycler: PacketBatchRecycler,
        shred_version: u16,
        name: &'static str,
        flags: PacketFlags,
    ) -> (Vec<JoinHandle<()>>, JoinHandle<()>) {
        let (packet_sender, packet_receiver) = unbounded();
        let streamers = sockets
            .into_iter()
            .map(|s| {
                streamer::receiver(
                    s,
                    exit.clone(),
                    packet_sender.clone(),
                    recycler.clone(),
                    Arc::new(StreamerReceiveStats::new("packet_modifier")),
                    PACKET_COALESCE_DURATION,
                    true, // use_pinned_memory
                    None, // in_vote_only_mode
                )
            })
            .collect();
        let modifier_hdl = Builder::new()
            .name("solTvuFetchPMod".to_string())
            .spawn(move || {
                Self::modify_packets(
                    packet_receiver,
                    shred_version,
                    name,
                    flags,
                )
            })
            .unwrap();
        (streamers, modifier_hdl)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        sockets: Vec<Arc<UdpSocket>>,
        turbine_quic_endpoint_receiver: Receiver<(Pubkey, SocketAddr, Bytes)>,
        shred_version: u16,
        exit: Arc<AtomicBool>,
    ) -> Self {
        let recycler = PacketBatchRecycler::warmed(100, 1024);

        let (mut tvu_threads, tvu_filter) = Self::packet_modifier(
            sockets,
            exit.clone(),
            recycler.clone(),
            shred_version,
            "shred_fetch",
            PacketFlags::empty(),
        );

        tvu_threads.push(tvu_filter);

        // Turbine shreds fetched over QUIC protocol.
        tvu_threads.extend([
            Builder::new()
                .name("solTvuRecvQuic".to_string())
                .spawn(|| {
                    varun_receive_quic_datagrams(
                        turbine_quic_endpoint_receiver,
                        recycler,
                        exit,
                    )
                })
                .unwrap(),
        ]);
        Self {
            thread_hdls: tvu_threads,
        }
    }

    pub(crate) fn join(self) -> thread::Result<()> {
        for thread_hdl in self.thread_hdls {
            thread_hdl.join()?;
        }
        Ok(())
    }
}
// Accepts shreds in the slot range [root + 1, max_slot].

fn varun_should_discard_shred(
    packet: &Packet,
) -> bool {
    let shred = match layout::get_shred(packet) {
        None => {
            return true;
        }
        Some(shred) => {
            // println!("inpacket");
            let x = shred;
            x
        },
    };
    return false
}


fn varun_receive_quic_datagrams(
    turbine_quic_endpoint_receiver: Receiver<(Pubkey, SocketAddr, Bytes)>,
    recycler: PacketBatchRecycler,
    exit: Arc<AtomicBool>,
) {
    const RECV_TIMEOUT: Duration = Duration::from_secs(1);
    while !exit.load(Ordering::Relaxed) {
        let entry = match turbine_quic_endpoint_receiver.recv_timeout(RECV_TIMEOUT) {
            Ok(entry) => entry,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        let mut packet_batch =
            PacketBatch::new_with_recycler(&recycler, PACKETS_PER_BATCH, "varun_receive_quic_datagrams");
        unsafe {
            packet_batch.set_len(PACKETS_PER_BATCH);
        };
        let deadline = Instant::now() + PACKET_COALESCE_DURATION;
        let entries = std::iter::once(entry).chain(
            std::iter::repeat_with(|| turbine_quic_endpoint_receiver.recv_deadline(deadline).ok())
                .while_some(),
        );
        let size = entries
            .filter(|(_, _, bytes)| bytes.len() <= PACKET_DATA_SIZE)
            .zip(packet_batch.iter_mut())
            .map(|((_pubkey, addr, bytes), packet)| {
                println!("packetreceived3");
                *packet.meta_mut() = Meta {
                    size: bytes.len(),
                    addr: addr.ip(),
                    port: addr.port(),
                    flags: PacketFlags::empty(),
                };
                packet.buffer_mut()[..bytes.len()].copy_from_slice(&bytes);
            })
            .count();
        if size > 0 {
            packet_batch.truncate(size);
            // if sender.send(packet_batch).is_err() {
            //     return;
            // }
        }
    }
}

pub(crate) fn receive_repair_quic_packets(
    repair_quic_endpoint_receiver: Receiver<(SocketAddr, Vec<u8>)>,
    sender: Sender<PacketBatch>,
    recycler: PacketBatchRecycler,
    exit: Arc<AtomicBool>,
) {
    const RECV_TIMEOUT: Duration = Duration::from_secs(1);
    while !exit.load(Ordering::Relaxed) {
        let entry = match repair_quic_endpoint_receiver.recv_timeout(RECV_TIMEOUT) {
            Ok(entry) => entry,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        let mut packet_batch =
            PacketBatch::new_with_recycler(&recycler, PACKETS_PER_BATCH, "varun_receive_quic_datagrams");
        unsafe {
            packet_batch.set_len(PACKETS_PER_BATCH);
        };
        let deadline = Instant::now() + PACKET_COALESCE_DURATION;
        let entries = std::iter::once(entry).chain(
            std::iter::repeat_with(|| repair_quic_endpoint_receiver.recv_deadline(deadline).ok())
                .while_some(),
        );
        let size = entries
            .filter(|(_, bytes)| bytes.len() <= PACKET_DATA_SIZE)
            .zip(packet_batch.iter_mut())
            .map(|((addr, bytes), packet)| {
                *packet.meta_mut() = Meta {
                    size: bytes.len(),
                    addr: addr.ip(),
                    port: addr.port(),
                    flags: PacketFlags::REPAIR,
                };
                packet.buffer_mut()[..bytes.len()].copy_from_slice(&bytes);
            })
            .count();
        if size > 0 {
            packet_batch.truncate(size);
            if sender.send(packet_batch).is_err() {
                return; // The receiver end of the channel is disconnected.
            }
        }
    }
}

#[must_use]
fn should_drop_legacy_shreds(
    shred_slot: Slot,
    feature_set: &FeatureSet,
    epoch_schedule: &EpochSchedule,
) -> bool {
    match feature_set.activated_slot(&feature_set::drop_legacy_shreds::id()) {
        None => false,
        Some(feature_slot) => {
            let feature_epoch = epoch_schedule.get_epoch(feature_slot);
            let shred_epoch = epoch_schedule.get_epoch(shred_slot);
            feature_epoch < shred_epoch
        }
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        solana_ledger::{
            blockstore::MAX_DATA_SHREDS_PER_SLOT,
            shred::{ReedSolomonCache, Shred, ShredFlags},
        },
        solana_sdk::packet::Packet,
    };

    #[test]
    fn test_data_code_same_index() {
        solana_logger::setup();
        let mut packet = Packet::default();
        let mut stats = ShredFetchStats::default();

        let slot = 2;
        let shred_version = 45189;
        let shred = Shred::new_from_data(
            slot,
            3,   // shred index
            1,   // parent offset
            &[], // data
            ShredFlags::LAST_SHRED_IN_SLOT,
            0, // reference_tick
            shred_version,
            3, // fec_set_index
        );
        shred.copy_to_packet(&mut packet);

        let last_root = 0;
        let last_slot = 100;
        let slots_per_epoch = 10;
        let max_slot = last_slot + 2 * slots_per_epoch;
        assert!(!should_discard_shred(
            &packet,
            last_root,
            max_slot,
            shred_version,
            |_| false, // should_drop_legacy_shreds
            &mut stats,
        ));
        let coding = solana_ledger::shred::Shredder::generate_coding_shreds(
            &[shred],
            3, // next_code_index
            &ReedSolomonCache::default(),
        );
        coding[0].copy_to_packet(&mut packet);
        assert!(!should_discard_shred(
            &packet,
            last_root,
            max_slot,
            shred_version,
            |_| false, // should_drop_legacy_shreds
            &mut stats,
        ));
    }

    #[test]
    fn test_shred_filter() {
        solana_logger::setup();
        let mut packet = Packet::default();
        let mut stats = ShredFetchStats::default();
        let last_root = 0;
        let last_slot = 100;
        let slots_per_epoch = 10;
        let shred_version = 59445;
        let max_slot = last_slot + 2 * slots_per_epoch;

        // packet size is 0, so cannot get index
        assert!(should_discard_shred(
            &packet,
            last_root,
            max_slot,
            shred_version,
            |_| false, // should_drop_legacy_shreds
            &mut stats,
        ));
        assert_eq!(stats.index_overrun, 1);
        let shred = Shred::new_from_data(
            2,   // slot
            3,   // index
            1,   // parent_offset
            &[], // data
            ShredFlags::LAST_SHRED_IN_SLOT,
            0, // reference_tick
            shred_version,
            0, // fec_set_index
        );
        shred.copy_to_packet(&mut packet);

        // rejected slot is 2, root is 3
        assert!(should_discard_shred(
            &packet,
            3,
            max_slot,
            shred_version,
            |_| false, // should_drop_legacy_shreds
            &mut stats,
        ));
        assert_eq!(stats.slot_out_of_range, 1);

        assert!(should_discard_shred(
            &packet,
            last_root,
            max_slot,
            345,       // shred_version
            |_| false, // should_drop_legacy_shreds
            &mut stats,
        ));
        assert_eq!(stats.shred_version_mismatch, 1);

        // Accepted for 1,3
        assert!(!should_discard_shred(
            &packet,
            last_root,
            max_slot,
            shred_version,
            |_| false, // should_drop_legacy_shreds
            &mut stats,
        ));

        let shred = Shred::new_from_data(
            1_000_000,
            3,
            0,
            &[],
            ShredFlags::LAST_SHRED_IN_SLOT,
            0,
            0,
            0,
        );
        shred.copy_to_packet(&mut packet);

        // Slot 1 million is too high
        assert!(should_discard_shred(
            &packet,
            last_root,
            max_slot,
            shred_version,
            |_| false, // should_drop_legacy_shreds
            &mut stats,
        ));

        let index = MAX_DATA_SHREDS_PER_SLOT as u32;
        let shred = Shred::new_from_data(5, index, 0, &[], ShredFlags::LAST_SHRED_IN_SLOT, 0, 0, 0);
        shred.copy_to_packet(&mut packet);
        assert!(should_discard_shred(
            &packet,
            last_root,
            max_slot,
            shred_version,
            |_| false, // should_drop_legacy_shreds
            &mut stats,
        ));
    }
}
