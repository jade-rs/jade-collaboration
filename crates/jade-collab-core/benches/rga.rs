//! How a story's RGA scales: building and walking a 100k-character story,
//! typing and deleting at random places in it, and the id ↔ index lookups the
//! models make on every edit.
//!
//! `cargo bench -p jade-collab-core --bench rga`

use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion};
use jade_collab_core::id::{Clock, Id, ReplicaId};
use jade_collab_core::rga::Rga;

const STORY: usize = 100_000;
const EDITS: usize = 1_000;

/// A deterministic xorshift stream, so every run touches the same positions.
struct Positions(u64);

impl Positions {
    fn new() -> Self {
        Self(0x9e37_79b9_7f4a_7c15)
    }

    fn below(&mut self, bound: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % bound as u64) as usize
    }
}

fn letter(k: usize) -> char {
    (b'a' + (k % 26) as u8) as char
}

/// A story loaded from a file: one run from the origin replica.
fn loaded_story() -> Rga<char> {
    let mut rga = Rga::new();
    rga.insert_run(Id::new(1, ReplicaId::ORIGIN), None, (0..STORY).map(letter))
        .unwrap();
    rga
}

fn build(c: &mut Criterion) {
    let mut group = c.benchmark_group("rga/build_100k");
    group.sample_size(10);
    group.bench_function("insert_run", |b| b.iter(|| black_box(loaded_story())));
    group.bench_function("type_each_char", |b| {
        b.iter(|| {
            let mut rga = Rga::new();
            let mut parent = None;
            for k in 0..STORY {
                let id = Id::new(k as u64 + 1, ReplicaId(1));
                rga.insert(id, parent, letter(k)).unwrap();
                parent = Some(id);
            }
            black_box(rga)
        })
    });
    group.finish();
}

fn edit(c: &mut Criterion) {
    let story = loaded_story();
    let mut group = c.benchmark_group("rga/edit_100k");
    group.sample_size(10);
    group.bench_function("type_1k_random", |b| {
        b.iter_batched(
            || story.clone(),
            |mut rga| {
                let mut positions = Positions::new();
                let mut clock = Clock::new(ReplicaId(2));
                clock.observe_lamport(rga.max_lamport());
                for k in 0..EDITS {
                    let at = positions.below(rga.visible_len() + 1);
                    let parent = at.checked_sub(1).map(|i| rga.visible_id_at(i).unwrap());
                    rga.insert(clock.tick(), parent, letter(k)).unwrap();
                }
                rga
            },
            BatchSize::LargeInput,
        )
    });
    group.bench_function("delete_1k_random", |b| {
        b.iter_batched(
            || story.clone(),
            |mut rga| {
                let mut positions = Positions::new();
                for _ in 0..EDITS {
                    let id = rga
                        .visible_id_at(positions.below(rga.visible_len()))
                        .unwrap();
                    rga.delete(id).unwrap();
                }
                rga
            },
            BatchSize::LargeInput,
        )
    });
    group.finish();
}

fn lookup(c: &mut Criterion) {
    // A story with tombstones and interleaved runs, so lookups do not see a
    // single contiguous run.
    let mut story = loaded_story();
    let mut positions = Positions::new();
    let mut clock = Clock::new(ReplicaId(3));
    clock.observe_lamport(story.max_lamport());
    for k in 0..EDITS {
        let parent = story.visible_id_at(positions.below(story.visible_len()));
        story.insert(clock.tick(), parent, letter(k)).unwrap();
        let victim = story
            .visible_id_at(positions.below(story.visible_len()))
            .unwrap();
        story.delete(victim).unwrap();
    }
    let ids = story.ids();
    let mut picks = Positions::new();
    let probe_ids: Vec<Id> = (0..EDITS).map(|_| ids[picks.below(ids.len())]).collect();
    let probe_indices: Vec<usize> = (0..EDITS)
        .map(|_| picks.below(story.visible_len()))
        .collect();

    let mut group = c.benchmark_group("rga/lookup_100k");
    group.sample_size(10);
    group.bench_function("position_1k", |b| {
        b.iter(|| {
            probe_ids
                .iter()
                .map(|id| story.position(*id).unwrap())
                .sum::<usize>()
        })
    });
    group.bench_function("visible_index_1k", |b| {
        b.iter(|| {
            probe_ids
                .iter()
                .filter_map(|id| story.visible_index(*id))
                .sum::<usize>()
        })
    });
    group.bench_function("visible_id_at_1k", |b| {
        b.iter(|| {
            probe_indices
                .iter()
                .map(|i| story.visible_id_at(*i).unwrap().lamport)
                .sum::<u64>()
        })
    });
    group.bench_function("visible_walk", |b| {
        b.iter(|| story.visible().map(|(_, c)| *c as u32 as u64).sum::<u64>())
    });
    group.finish();
}

criterion_group!(benches, build, edit, lookup);
criterion_main!(benches);
