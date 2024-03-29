use {
    crossbeam_channel::{Receiver, RecvTimeoutError, SendError},
    rayon::{prelude::*, ThreadPool, ThreadPoolBuilder}
    ,
    solana_ledger::{
        leader_schedule_cache::LeaderScheduleCache, shred,
    },
    solana_perf::{self, deduper::Deduper, packet::PacketBatch},
    solana_rayon_threadlimit::get_thread_count,
    solana_runtime::{bank::Bank, bank_forks::BankForks}
    ,
    std::{
        sync::Arc,
        thread::{Builder, JoinHandle},
        time::Duration,
    },
};
use solana_ledger::shred::build_dumped_shred;
use solana_ledger::varun_shard_data_cache;
use solana_ledger::varun_shard_data_cache::VarunShardDataCache;

const DEDUPER_FALSE_POSITIVE_RATE: f64 = 0.001;
const DEDUPER_NUM_BITS: u64 = 637_534_199; // 76MB
const DEDUPER_RESET_CYCLE: Duration = Duration::from_secs(5 * 60);

#[allow(clippy::enum_variant_names)]
enum Error {
    RecvDisconnected,
    RecvTimeout,
    SendError,
}

pub fn clone_spawn_shred_sigverify(
    shred_fetch_receiver: Receiver<PacketBatch>,
    varun_shred_cache: Arc<VarunShardDataCache>,
    // retransmit_sender: Sender<Vec</*shred:*/ Vec<u8>>>,
    // verified_sender: Sender<Vec<PacketBatch>>,
) -> JoinHandle<()> {
    let thread_pool = ThreadPoolBuilder::new()
        .num_threads(get_thread_count())
        .thread_name(|i| format!("solCloneSvrfyShred{i:02}"))
        .build()
        .unwrap();
    let run_shred_sigverify = move || {
        let mut rng = rand::thread_rng();
        let mut deduper = Deduper::<2, [u8]>::new(&mut rng, DEDUPER_NUM_BITS);
        loop {
            if deduper.maybe_reset(&mut rng, DEDUPER_FALSE_POSITIVE_RATE, DEDUPER_RESET_CYCLE) {
            }
            match clone_run_shred_sigverify(
                &thread_pool,
                &deduper,
                &shred_fetch_receiver,
                varun_shred_cache.clone()
                // &retransmit_sender,
                // &verified_sender,
            ) {
                Ok(()) => (),
                Err(Error::RecvTimeout) => (),
                Err(Error::RecvDisconnected) => break,
                Err(Error::SendError) => break,
            }
        }
    };
    Builder::new()
        .name("solClonedShredVerifr".to_string())
        .spawn(run_shred_sigverify)
        .unwrap()
}

#[allow(clippy::too_many_arguments)]
fn clone_run_shred_sigverify<const K: usize>(
    thread_pool: &ThreadPool,
    deduper: &Deduper<K, [u8]>,
    shred_fetch_receiver: &Receiver<PacketBatch>,
    varun_shred_cache: Arc<VarunShardDataCache>,
    // retransmit_sender: &Sender<Vec</*shred:*/ Vec<u8>>>,
    // verified_sender: &Sender<Vec<PacketBatch>>,
) -> Result<(), Error> {
    const RECV_TIMEOUT: Duration = Duration::from_secs(1);
    let packets = shred_fetch_receiver.recv_timeout(RECV_TIMEOUT)?;
    let mut packets: Vec<_> = std::iter::once(packets)
        .chain(shred_fetch_receiver.try_iter())
        .collect();
    // let now = Instant::now();
    // TODO - SEE WHAT TO DO ABOUT THIS CODE?
    _ = thread_pool.install(|| {
        packets
            .par_iter()
            .flatten()
            // TODO - WHY DOES THIS FILTER THE SHREDS OUT?
            // .filter(|packet| {
            //     !packet.meta().discard()
            //         && packet
            //             .data(..)
            //             .map(|data| deduper.dedup(data))
            //             .unwrap_or(true)
            // })
            .filter_map(
                |packet|
                    shred::layout::get_shred(packet))
                    // packet.meta_mut().set_discard(true))
            .map(<[u8]>::to_vec)
            .filter_map(|s|shred::Shred::new_from_serialized_shred(s).ok())
            .for_each(|s| varun_shred_cache.process_shred(s))
            // .map(|s|build_dumped_shred(&s,0,0,0))
            // .map(|ds| {
            //     println!("{:#?}", ds);
            //     ds
            // .collect()
            // })
    });
    // clone_verify_packets(
    //     thread_pool,
    //     recycler_cache,
    //     &mut packets,
    // );
    // Exclude repair packets from retransmit.
    // let shreds: Vec<_> = packets
    //     .iter()
    //     .flat_map(PacketBatch::iter)
    //     .filter(|packet| !packet.meta().discard() && !packet.meta().repair())
    //     .filter_map(shred::layout::get_shred)
    //     .map(<[u8]>::to_vec)
    //     .collect();


    // retransmit_sender.send(shreds)?;
    // verified_sender.send(packets)?;
    Ok(())
}

// fn clone_verify_packets(
//     thread_pool: &ThreadPool,
//     recycler_cache: &RecyclerCache,
//     packets: &mut [PacketBatch],
// ) {
//     let working_bank = bank_forks.read().unwrap().working_bank();
//
//     let out = verify_shreds_gpu(thread_pool, packets, &leader_slots, recycler_cache);
//     solana_perf::sigverify::mark_disabled(packets, &out);
// }

// // Returns pubkey of leaders for shred slots refrenced in the packets.
// // Marks packets as discard if:
// //   - fails to deserialize the shred slot.
// //   - slot leader is unknown.
// //   - slot leader is the node itself (circular transmission).
// fn get_slot_leaders(
//     self_pubkey: &Pubkey,
//     batches: &mut [PacketBatch],
//     leader_schedule_cache: &LeaderScheduleCache,
//     bank: &Bank,
// ) -> HashMap<Slot, Option<Pubkey>> {
//     let mut leaders = HashMap::<Slot, Option<Pubkey>>::new();
//     batches
//         .iter_mut()
//         .flat_map(PacketBatch::iter_mut)
//         .filter(|packet| !packet.meta().discard())
//         .filter(|packet| {
//             let shred = shred::layout::get_shred(packet);
//             let Some(slot) = shred.and_then(shred::layout::get_slot) else {
//                 return true;
//             };
//             leaders
//                 .entry(slot)
//                 .or_insert_with(|| {
//                     // Discard the shred if the slot leader is the node itself.
//                     leader_schedule_cache
//                         .slot_leader_at(slot, Some(bank))
//                         .filter(|leader| leader != self_pubkey)
//                 })
//                 .is_none()
//         })
//         .for_each(|packet| packet.meta_mut().set_discard(true));
//     leaders
// }
//
// fn count_discards(packets: &[PacketBatch]) -> usize {
//     packets
//         .iter()
//         .flat_map(PacketBatch::iter)
//         .filter(|packet| packet.meta().discard())
//         .count()
// }

impl From<RecvTimeoutError> for Error {
    fn from(err: RecvTimeoutError) -> Self {
        match err {
            RecvTimeoutError::Timeout => Self::RecvTimeout,
            RecvTimeoutError::Disconnected => Self::RecvDisconnected,
        }
    }
}

impl<T> From<SendError<T>> for Error {
    fn from(_: SendError<T>) -> Self {
        Self::SendError
    }
}

// struct CloneShredSigVerifyStats {
//     since: Instant,
//     num_iters: usize,
//     num_packets: usize,
//     num_deduper_saturations: usize,
//     num_discards_post: usize,
//     num_discards_pre: usize,
//     num_duplicates: usize,
//     num_retransmit_shreds: usize,
//     elapsed_micros: u64,
// }

// impl CloneShredSigVerifyStats {
//     const METRICS_SUBMIT_CADENCE: Duration = Duration::from_secs(2);
//
//     fn new(now: Instant) -> Self {
//         Self {
//             since: now,
//             num_iters: 0usize,
//             num_packets: 0usize,
//             num_discards_pre: 0usize,
//             num_deduper_saturations: 0usize,
//             num_discards_post: 0usize,
//             num_duplicates: 0usize,
//             num_retransmit_shreds: 0usize,
//             elapsed_micros: 0u64,
//         }
//     }
//
//     fn maybe_submit(&mut self) {
//         if self.since.elapsed() <= Self::METRICS_SUBMIT_CADENCE {
//             return;
//         }
//         datapoint_info!(
//             "shred_sigverify",
//             ("num_iters", self.num_iters, i64),
//             ("num_packets", self.num_packets, i64),
//             ("num_discards_pre", self.num_discards_pre, i64),
//             ("num_deduper_saturations", self.num_deduper_saturations, i64),
//             ("num_discards_post", self.num_discards_post, i64),
//             ("num_duplicates", self.num_duplicates, i64),
//             ("num_retransmit_shreds", self.num_retransmit_shreds, i64),
//             ("elapsed_micros", self.elapsed_micros, i64),
//         );
//         *self = Self::new(Instant::now());
//     }
// }

#[cfg(test)]
mod tests {
    use {
        solana_ledger::{
            genesis_utils::create_genesis_config_with_leader,
            shred::{Shred, ShredFlags},
        },
        solana_perf::packet::Packet,
        solana_runtime::bank::Bank,
        solana_sdk::signature::{Keypair, Signer},
        super::*,
    };

    #[test]
    fn test_sigverify_shreds_verify_batches() {
        let leader_keypair = Arc::new(Keypair::new());
        let leader_pubkey = leader_keypair.pubkey();
        let bank = Bank::new_for_tests(
            &create_genesis_config_with_leader(100, &leader_pubkey, 10).genesis_config,
        );
        let leader_schedule_cache = LeaderScheduleCache::new_from_bank(&bank);
        let bank_forks = BankForks::new_rw_arc(bank);
        let batch_size = 2;
        let mut batch = PacketBatch::with_capacity(batch_size);
        batch.resize(batch_size, Packet::default());
        let mut batches = vec![batch];

        let mut shred = Shred::new_from_data(
            0,
            0xc0de,
            0xdead,
            &[1, 2, 3, 4],
            ShredFlags::LAST_SHRED_IN_SLOT,
            0,
            0,
            0xc0de,
        );
        shred.sign(&leader_keypair);
        batches[0][0].buffer_mut()[..shred.payload().len()].copy_from_slice(shred.payload());
        batches[0][0].meta_mut().size = shred.payload().len();

        let mut shred = Shred::new_from_data(
            0,
            0xbeef,
            0xc0de,
            &[1, 2, 3, 4],
            ShredFlags::LAST_SHRED_IN_SLOT,
            0,
            0,
            0xc0de,
        );
        let wrong_keypair = Keypair::new();
        shred.sign(&wrong_keypair);
        batches[0][1].buffer_mut()[..shred.payload().len()].copy_from_slice(shred.payload());
        batches[0][1].meta_mut().size = shred.payload().len();

        let thread_pool = ThreadPoolBuilder::new().num_threads(3).build().unwrap();
        // clone_verify_packets(
        //     &thread_pool,
        //     &Pubkey::new_unique(), // self_pubkey
        //     &bank_forks,
        //     &leader_schedule_cache,
        //     &RecyclerCache::warmed(),
        //     &mut batches,
        // );
        assert!(!batches[0][0].meta().discard());
        assert!(batches[0][1].meta().discard());
    }
}
