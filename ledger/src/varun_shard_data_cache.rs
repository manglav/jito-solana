use crate::shred::{LEGACY_SHRED_DATA_CAPACITY, max_entries_per_n_shred, ProcessShredsStats, ReedSolomonCache, Shredder};
use crate::shred::{Shred, ShredData};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread;
// use solana_ledger::shredder;

use crossbeam::channel::{Receiver, Sender};
use dashmap::DashMap;
use dashmap::mapref::one::RefMut;
// use solana_ledger::shred::shred_code::ShredCode;
// use solana_ledger::shred::{LEGACY_SHRED_DATA_CAPACITY, max_entries_per_n_shred, ProcessShredsStats, ReedSolomonCache, Shred, ShredData, Shredder};
use solana_sdk::clock::Slot;
// use shred:

use solana_entry::entry::Entry;
use solana_perf::test_tx;
use solana_sdk::hash::Hash;
use solana_sdk::signature::Keypair;
use solana_sdk::signature::Signer;
use crate::shred::shred_code::ShredCode;


const MAX_SLOT_DISTANCE: u64 = 50;
const UNKNOWN_INDEX: u32 = 999999;

struct PacketBatch {
    data_shreds: BTreeMap<u32, ShredData>,
    code_shreds: BTreeMap<u32, ShredCode>,
    num_data_shreds: Arc<AtomicU32>,
    batch_complete: Arc<AtomicBool>,
    marked_full: Arc<AtomicBool>,
    start_data_index: u32,
    end_data_index: u32,
}

pub struct VarunShardDataCache {
    packet_batches: DashMap<(u64, u8), PacketBatch>,
    batch_complete_sender: Sender<(u64, u8)>,
}

impl VarunShardDataCache {
    pub fn new(batch_complete_sender: Sender<(u64, u8)>) -> Self {
        VarunShardDataCache {
            packet_batches: DashMap::new(),
            batch_complete_sender,
        }
    }

    pub fn process_shred(&self, shred: Shred) {
        match shred {
            Shred::ShredData(ref data_shred) => {
                let key = (shred.slot(), data_shred.reference_tick());

                // Update packet batch information
                let mut packet_batch = self.packet_batches.entry(key).or_insert(PacketBatch {
                    data_shreds: BTreeMap::new(),
                    code_shreds: BTreeMap::new(),
                    num_data_shreds: Arc::new(AtomicU32::new(0)),
                    batch_complete: Arc::new(AtomicBool::new(false)),
                    start_data_index: UNKNOWN_INDEX,
                    end_data_index: UNKNOWN_INDEX, // end data index can never be 0, so use as marker for unknown
                    marked_full: Arc::new(AtomicBool::new(false)),
                });


                // if shred is already added, skip
                // if the key already exists in data shreds, no need to re-process
                if packet_batch.data_shreds.contains_key(&shred.index()) {
                    return
                }

                //// CASES
                // Packet is either the beginning of a batch, middle, or end
                // Packet can be beginning of slot, or end of slot
                // A packet batch is complete if you have the beginning, the end, and everything in the middle


                // exit early if packet batch is already complete
                if packet_batch.marked_full.load(Ordering::SeqCst) {
                    return
                }

                //
                // the end of a packet batch is clearly marked
                // the beginning of a batch is marked via the FEC_Set index - refers to data_index of first
                // packet in batch.
                packet_batch.data_shreds.insert(shred.index(), data_shred.clone());

                // when setting start_data_index, should always be the same value
                if (packet_batch.start_data_index != UNKNOWN_INDEX && packet_batch.start_data_index != shred.fec_set_index()) {
                    error!("mismatched start_data_index found");
                }
                // IMPORTANT - there can be multiple FEC sets per batch tick, so take the minimum value to determine start of batch...
                // BUT NOW WE DON'T KNOW IF IT's ACCURATE EVEN IF IT's SET
                packet_batch.start_data_index = shred.fec_set_index();

                error!("slot: {}, batch_tick: {}, shard_index:{}, fec_index:{}, last_in_batch:{}",
                    shred.slot(),
                    shred.reference_tick(),
                    shred.index(),
                    shred.fec_set_index(),
                    shred.data_complete()
                );

                if shred.data_complete() {
                    packet_batch.batch_complete.store(true, Ordering::SeqCst);
                    packet_batch.end_data_index = shred.index();
                }

                // get start index of batch - do we really care about this? Let's assume no
                // can optimize this by putting packet batches into a B-Tree, to find previous complete btree
                // otherwise get num_data_shreds from FEC shred and subract from end_data_index
                // if shred.index() > 0 && packet_batch.end_data_index > 0 {
                // }


                if check_completed_batch(&packet_batch) {
                    packet_batch.marked_full.store(true, Ordering::SeqCst);
                    println!("cache full batch notifier worked, got key {:?}", key);
                    // if self.batch_complete_sender.send(key).is_err() {
                    //     println!("Failed to send batch complete signal");
                    // }
                }

            }
            Shred::ShredCode(ref code_shred) => {
                // batch tick number is always 63 for coding shreds
                let key = (shred.slot(), shred.reference_tick());

                // Update packet batch information
                let mut packet_batch = self.packet_batches.entry(key).or_insert(PacketBatch {
                    data_shreds: BTreeMap::new(),
                    code_shreds: BTreeMap::new(),
                    num_data_shreds: Arc::new(AtomicU32::new(0)),
                    batch_complete: Arc::new(AtomicBool::new(false)),
                    marked_full: Arc::new(AtomicBool::new(false)),
                    start_data_index: UNKNOWN_INDEX,
                    end_data_index: UNKNOWN_INDEX, // end data index can never be 0, so use as marker for unknown
                });


                // exit early if packet batch is already complete
                if packet_batch.marked_full.load(Ordering::SeqCst) {
                    return
                }

                // if shred is already added, skip
                // if the key already exists in data shreds, no need to re-process
                if packet_batch.code_shreds.contains_key(&shred.index()) {
                    return
                }

                //// CASES
                // Packet is either the beginning of a batch, middle, or end
                // Packet can be beginning of slot, or end of slot
                // A packet batch is complete if you have the beginning, the end, and everything in the middle


                packet_batch.code_shreds.insert(shred.index(), code_shred.clone());

                // Update the number of data shreds if available
                if code_shred.num_data_shreds() > 0 {
                    packet_batch.num_data_shreds.store(code_shred.num_data_shreds() as u32, Ordering::SeqCst);
                }

                if check_completed_batch(&packet_batch) {
                    packet_batch.marked_full.store(true, Ordering::SeqCst);
                    println!("cache full batch notifier worked, got key {:?}", key);
                    // if self.batch_complete_sender.send(key).is_err() {
                    //     println!("Failed to send batch complete signal");
                    // }
                }
            }
        }
    }

    fn clean_old_slots(&self, current_slot: u64) {
        self.packet_batches.retain(|&(slot, _), _| current_slot - slot <= MAX_SLOT_DISTANCE);
    }


}

fn check_completed_batch(packet_batch: &RefMut<(u64, u8), PacketBatch>) -> bool {
    // mark packetbatch as "complete" if it has
    // We do care if the packet batch is complete
    // this comparison only works if a code shred is present
    let x:bool = packet_batch.data_shreds.len() == (*packet_batch.num_data_shreds).load(Ordering::SeqCst) as usize;
    return x;
    // check if both indexes are present
    let y:bool = (packet_batch.end_data_index != UNKNOWN_INDEX && packet_batch.start_data_index != UNKNOWN_INDEX);

    // log both
    if packet_batch.end_data_index <= packet_batch.start_data_index {
        error!("index error, end:{} vs start:{}", packet_batch.end_data_index, packet_batch.start_data_index )
    }
    let z:bool = packet_batch.data_shreds.len() == (1usize) + (packet_batch.end_data_index - packet_batch.start_data_index) as usize;


    (x || (y && z))
}


#[cfg(test)]
mod tests {
    use solana_sdk::system_transaction;
use super::*;

    #[test]
    fn test_process_shred() {
        let (batch_complete_sender, batch_complete_receiver) = crossbeam::channel::unbounded();
        let shard_data_model = Arc::new(VarunShardDataCache::new(batch_complete_sender));

        let keypair = Arc::new(Keypair::new());
        let slot = 1;
        let parent_slot = 0;
        // THIS NUMBER CAN'T BE TOO HIGH, THERE ARE LIMITS!
        let number_of_fake_entries = 100; // with two entries, can fit into one shred

        // to generate bytes, bincode::serialize(&[Entry])

        let (entries, shreds) = tests::generate_entry_batch_and_shreds(
            keypair, slot, parent_slot, number_of_fake_entries);

        // Now try to take one Shred and extract one entry
        println!("length of entries {}", entries.len());
        println!("length of shreds {}", shreds.len());

        // let x = generate_entry_batch_and_shreds()
        // let a = make_shreds(5);
        let shred1 = &shreds[0];
        let shred2 = &shreds[1];
        let key1 = (shred1.slot(), shred1.reference_tick());
        let key2 = (shred2.slot(), shred2.reference_tick());
        let packet1_index = shred1.index();
        let packet2_index = shred2.index();
        // let shred1 = DataShred {
        //     slot: 1,
        //     shred_index: 0,
        //     batch_tick: 0,
        //     block_complete: false,
        //     batch_complete: false,
        //     data: vec![1, 2, 3],
        // };
        // let shred2 = DataShred {
        //     slot: 1,
        //     shred_index: 1,
        //     batch_tick: 0,
        //     block_complete: false,
        //     batch_complete: true,
        //     data: vec![4, 5, 6],
        // };

        // for shred_a in a {
        //     shard_data_model.process_shred(shred_a.clone());
        // }
        shard_data_model.process_shred(shred1.clone());
        shard_data_model.process_shred(shred2.clone());

        assert_eq!(shard_data_model.packet_batches.len(), 1);
        assert!(shard_data_model.packet_batches.contains_key(&(key1)));

        let packet_batch = shard_data_model.packet_batches.get(&(key1)).unwrap();
        assert_eq!(packet_batch.data_shreds.len(), 2);
        assert_eq!(
            Shred::ShredData(
                packet_batch.data_shreds.get(&packet1_index).unwrap().clone()
            ),
            shred1.clone()
        );
        assert_eq!(
            Shred::ShredData(packet_batch.data_shreds.get(&packet2_index).unwrap().clone()),
            shred2.clone());

        // let received_key = batch_complete_receiver.recv().unwrap();
        // // assert_eq!(received_key, (1, 0));
        // for shred in shreds[2..] {
        //     shard_data_model.process_shred(shred.clone());
        // }
        let (batch_complete_sender2, batch_complete_receiver2) = crossbeam::channel::unbounded();
        let shard_data_model2 = Arc::new(VarunShardDataCache::new(batch_complete_sender2));
        for shred in shreds.iter() {
            shard_data_model2.process_shred((shred).clone())
        }

        // Check if all shards are in the cache
        for shred in shreds.iter() {
            let x = shard_data_model2.packet_batches.get(
                &(shred.slot(), shred.reference_tick())
            );

            if x.is_none() {
                assert_eq!(1,2);
            } else {
                let z = x.unwrap();
                let y = z.data_shreds.get(&shred.index());
                if y.is_none() {
                    assert_eq!(3,4);
                } else {
                    println!("found shred {}", shred.index())
                }
            }

            println!("hello");
        }

        println!("waiting for batch_signal");
        // assert!(packet_batch.batch_complete.load(Ordering::SeqCst));
        let received_key = batch_complete_receiver2.recv().unwrap();
        assert_eq!(received_key, (1, 0));
    }

    fn shred_to_shred_data(shred: &Shred) -> Option<&ShredData> {
        match shred {
            Shred::ShredData(shred_data) => Some(shred_data),
            Shred::ShredCode(_) => None,
        }
    }

    fn generate_entry_batch_and_shreds(
        keypair: Arc<Keypair>,
        slot: Slot,
        parent_slot: Slot,
        number_of_fake_entries: i32) -> (Vec<Entry>, Vec<Shred>) {
        let shredder = Shredder::new(slot, parent_slot, 0, 0).unwrap();
        // construct entries with fake transactions
        let entries: Vec<_> = (0..number_of_fake_entries)
            .map(|_| {
                let keypair0 = Keypair::new();
                let keypair1 = Keypair::new();
                let tx0 =
                    system_transaction::transfer(&keypair0, &keypair1.pubkey(), 1, Hash::default());
                Entry::new(&Hash::default(), 1, vec![tx0])
            })
            .collect();

        let (data_shreds, _) = shredder.entries_to_shreds(
            &keypair,
            &entries,
            true, // is_last_in_slot
            0,    // next_shred_index
            0,    // next_code_index
            true, // merkle_variant
            &ReedSolomonCache::default(),
            &mut ProcessShredsStats::default(),
        );

        return (entries, data_shreds)
    }

    // #[test]
    // fn test_clean_old_slots() {
    //     let (batch_complete_sender, _) = crossbeam::channel::unbounded();
    //     let shard_data_model = Arc::new(ShardDataModel::new(batch_complete_sender));
    //
    //     let shred1 = DataShred {
    //         slot: 1,
    //         shred_index: 0,
    //         batch_tick: 0,
    //         block_complete: false,
    //         batch_complete: false,
    //         data: vec![1, 2, 3],
    //     };
    //     let shred2 = DataShred {
    //         slot: 100,
    //         shred_index: 0,
    //         batch_tick: 0,
    //         block_complete: false,
    //         batch_complete: false,
    //         data: vec![4, 5, 6],
    //     };
    //
    //     shard_data_model.process_shred(shred1);
    //     shard_data_model.process_shred(shred2);
    //
    //     assert_eq!(shard_data_model.packet_batches.len(), 2);
    //
    //     shard_data_model.clean_old_slots(150);
    //
    //     assert_eq!(shard_data_model.packet_batches.len(), 1);
    //     assert!(shard_data_model.packet_batches.contains_key(&(100, 0)));
    // }
}

fn make_test_entry(txs_per_entry: u64) -> Entry {
    Entry {
        num_hashes: 100_000,
        hash: Hash::default(),
        transactions: vec![test_tx::test_tx().into(); txs_per_entry as usize],
    }
}
fn make_large_unchained_entries(txs_per_entry: u64, num_entries: u64) -> Vec<Entry> {
    (0..num_entries)
        .map(|_| make_test_entry(txs_per_entry))
        .collect()
}

fn make_shreds(num_shreds: usize) -> Vec<Shred> {
    let txs_per_entry = 128;
    let num_entries = max_entries_per_n_shred(
        &make_test_entry(txs_per_entry),
        2 * num_shreds as u64,
        Some(LEGACY_SHRED_DATA_CAPACITY),
    );
    let entries = make_large_unchained_entries(txs_per_entry, num_entries);
    let shredder = Shredder::new(1, 0, 0, 0).unwrap();
    let (data_shreds, _) = shredder.entries_to_shreds(
        &Keypair::new(),
        &entries,
        true,  // is_last_in_slot
        0,     // next_shred_index
        0,     // next_code_index
        false, // merkle_variant
        &ReedSolomonCache::default(),
        &mut ProcessShredsStats::default(),
    );
    assert!(data_shreds.len() >= num_shreds);
    data_shreds
}


fn main() {
    // let (batch_complete_sender, batch_complete_receiver) = crossbeam::channel::unbounded();
    // let shard_data_model = Arc::new(ShardDataModel::new(batch_complete_sender));
    //
    // // Simulated incoming shreds
    // let shreds = vec![
    //     DataShred {
    //         slot: 1,
    //         shred_index: 0,
    //         batch_tick: 0,
    //         block_complete: false,
    //         batch_complete: false,
    //         data: vec![1, 2, 3],
    //     },
    //     DataShred {
    //         slot: 1,
    //         shred_index: 1,
    //         batch_tick: 0,
    //         block_complete: false,
    //         batch_complete: true,
    //         data: vec![4, 5, 6],
    //     },
    //     DataShred {
    //         slot: 2,
    //         shred_index: 0,
    //         batch_tick: 0,
    //         block_complete: true,
    //         batch_complete: true,
    //         data: vec![7, 8, 9],
    //     },
    // ];
    //
    // // Process incoming shreds
    // for shred in shreds {
    //     shard_data_model.process_shred(shred);
    // }
    //
    // // Start a thread to listen for batch complete signals
    // let batch_complete_receiver_thread = thread::spawn(move || {
    //     while let Ok((slot, batch_tick)) = batch_complete_receiver.recv() {
    //         println!("Batch complete: slot={}, batch_tick={}", slot, batch_tick);
    //     }
    // });
    //
    // // Simulated slot cleaning
    // let current_slot = 100;
    // shard_data_model.clean_old_slots(current_slot);
    //
    // // Wait for the batch complete receiver thread to finish
    // batch_complete_receiver_thread.join().unwrap();
}