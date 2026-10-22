//! Per-operation cost of the docx collaboration path on a large file.
//!
//! `cargo bench -p jade-docx-collab --bench docx_ops -- [fixture] [samples]`
//! (default `yokohama.docx`, 7 samples). Every figure is the median over the
//! samples of the mean cost of one operation in a sample, so a busy machine
//! moves it less than a mean would. Phases are timed apart, as the server
//! runs them: `admit` (sequenced only), the model's `apply`, jade's
//! `materialize` (plus what the server derives afterwards), and on the
//! client the intent API's `resolve`.

use std::time::{Duration, Instant};

use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_sync::{ApplyContext, Document, Lane, Materializer};
use jade_docx_collab::intent::{resolve, IntentOp};
use jade_docx_collab::{DocxModel, DocxOp, FormatChange, JadeDocx, ParagraphAlignment};
use jade_docx_document_core::word_document::ParagraphStory;

fn fixture(name: &str) -> Vec<u8> {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../jade-core");
    let path = root.join("public/fixtures").join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Phase times of one operation.
#[derive(Default, Clone, Copy)]
struct Phases {
    resolve: Duration,
    admit: Duration,
    apply: Duration,
    materialize: Duration,
}

impl Phases {
    fn add(&mut self, other: Phases) {
        self.resolve += other.resolve;
        self.admit += other.admit;
        self.apply += other.apply;
        self.materialize += other.materialize;
    }

    fn div(self, n: u32) -> Phases {
        Phases {
            resolve: self.resolve / n,
            admit: self.admit / n,
            apply: self.apply / n,
            materialize: self.materialize / n,
        }
    }
}

struct Session {
    model: DocxModel,
    jade: JadeDocx,
    next: u64,
}

impl Session {
    fn open(bytes: &[u8]) -> Self {
        let jade = JadeDocx::load(bytes).expect("jade loads the fixture");
        let model = DocxModel::from_word_document(jade.word()).expect("model builds");
        let next = model.max_lamport() + 1;
        Self { model, jade, next }
    }

    fn resolve(&mut self, intent: &IntentOp) -> Vec<DocxOp> {
        let floor = self.model.max_lamport() + 1;
        let next = &mut self.next;
        *next = (*next).max(floor);
        let mut ids = |n: u64| {
            let id = Id::new(*next, ReplicaId(1));
            *next += n;
            id
        };
        resolve(&self.model, intent, &mut ids).expect("the intent resolves")
    }

    /// Runs `intent` as a client and the server do, timing each phase.
    fn run(&mut self, intent: &IntentOp) -> Phases {
        let mut phases = Phases::default();
        let started = Instant::now();
        let ops = self.resolve(intent);
        phases.resolve = started.elapsed();
        for op in ops {
            phases.add(self.commit(&op));
        }
        phases
    }

    fn commit(&mut self, op: &DocxOp) -> Phases {
        let ctx = ApplyContext {
            origin: ReplicaId(1),
            base_seq: 0,
            seq: Some(1),
        };
        let mut phases = Phases::default();
        if DocxModel::lane(op) == Lane::Sequenced {
            let started = Instant::now();
            self.jade
                .admit(&self.model, op)
                .unwrap_or_else(|e| panic!("jade refused {op:?}: {e}"));
            phases.admit = started.elapsed();
        }
        let started = Instant::now();
        let effects = self
            .model
            .apply(op, &ctx)
            .unwrap_or_else(|e| panic!("{op:?}: {e}"));
        phases.apply = started.elapsed();
        let started = Instant::now();
        self.jade
            .materialize(&self.model, effects, &ctx)
            .unwrap_or_else(|e| panic!("{op:?}: {e}"));
        let origin = ApplyContext {
            origin: ReplicaId::ORIGIN,
            ..ctx
        };
        for derived in self.jade.derived(&self.model, op).unwrap() {
            let effects = self.model.apply(&derived, &origin).expect("derived");
            self.jade
                .materialize(&self.model, effects, &origin)
                .expect("derived");
        }
        phases.materialize = started.elapsed();
        phases
    }

    /// The ordinal of a body paragraph near the middle with some text.
    fn middle_paragraph(&self) -> usize {
        let paragraphs = self.model.paragraphs();
        let body: Vec<usize> = paragraphs
            .iter()
            .enumerate()
            .filter(|(_, p)| matches!(p.at, ParagraphStory::Body { .. }) && p.chars.len() >= 24)
            .map(|(n, _)| n)
            .collect();
        body[body.len() / 2]
    }

    /// The byte offset of character `n` of paragraph `p`.
    fn byte(&self, p: usize, n: usize) -> usize {
        let text = &self.model.paragraphs()[p].text;
        text.char_indices().nth(n).map_or(text.len(), |(at, _)| at)
    }
}

fn median(mut values: Vec<Duration>) -> Duration {
    values.sort();
    values[values.len() / 2]
}

fn report(name: &str, samples: Vec<Phases>) {
    let pick = |f: fn(&Phases) -> Duration| median(samples.iter().map(f).collect());
    let us = |d: Duration| d.as_secs_f64() * 1e6;
    let (resolve, admit, apply, materialize) = (
        pick(|p| p.resolve),
        pick(|p| p.admit),
        pick(|p| p.apply),
        pick(|p| p.materialize),
    );
    let server = admit + apply + materialize;
    println!(
        "{name:<28} {:>11.1} {:>11.1} {:>11.1} {:>11.1} {:>11.1}",
        us(resolve),
        us(admit),
        us(apply),
        us(materialize),
        us(server)
    );
}

fn main() {
    let args: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| !a.starts_with("--"))
        .collect();
    let name = args.first().map_or("yokohama.docx", String::as_str);
    let samples: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(7);
    let bytes = fixture(name);

    let started = Instant::now();
    let mut session = Session::open(&bytes);
    let load = started.elapsed();
    let paragraphs = session.model.paragraphs();
    let chars: usize = paragraphs.iter().map(|p| p.chars.len()).sum();
    println!(
        "{name}: {} paragraphs, {chars} characters, {} tables; load {:.1} ms; {samples} samples",
        paragraphs.len(),
        session.model.tables().len(),
        load.as_secs_f64() * 1e3
    );
    println!(
        "{:<28} {:>11} {:>11} {:>11} {:>11} {:>11}",
        "µs per operation (median)", "resolve", "admit", "apply", "materialize", "server"
    );

    // Typing: one character at a time at the end of what was typed.
    const KEYS: u32 = 40;
    let mut typing = Vec::new();
    for _ in 0..samples {
        let p = session.middle_paragraph();
        let mut total = Phases::default();
        for k in 0..KEYS as usize {
            total.add(session.run(&IntentOp::InsertText {
                paragraph: p,
                offset: session.byte(p, 4 + k),
                text: "a".to_string(),
            }));
        }
        typing.push(total.div(KEYS));
    }
    report("type a character", typing);

    // Paragraph split in the middle of a paragraph.
    const SPLITS: u32 = 10;
    let mut splits = Vec::new();
    for _ in 0..samples {
        let mut total = Phases::default();
        for _ in 0..SPLITS {
            let p = session.middle_paragraph();
            total.add(session.run(&IntentOp::InsertParagraphBreak {
                paragraph: p,
                offset: session.byte(p, 12),
                count: 1,
            }));
        }
        splits.push(total.div(SPLITS));
    }
    report("split a paragraph", splits);

    // Sequenced paragraph formatting.
    const FORMATS: u32 = 5;
    let mut formats = Vec::new();
    for s in 0..samples {
        let mut total = Phases::default();
        for k in 0..FORMATS as usize {
            let p = session.middle_paragraph();
            let alignment = if (s + k) % 2 == 0 {
                ParagraphAlignment::Center
            } else {
                ParagraphAlignment::Right
            };
            total.add(session.run(&IntentOp::FormatParagraphs {
                start_paragraph: p,
                end_paragraph: p,
                change: FormatChange::Alignment { alignment },
            }));
        }
        formats.push(total.div(FORMATS));
    }
    report("format (sequenced)", formats);

    // Sequenced character formatting (the base, as older clients send it).
    let mut char_formats = Vec::new();
    for s in 0..samples {
        let mut total = Phases::default();
        for k in 0..FORMATS as usize {
            let p = session.middle_paragraph();
            let ops = session.resolve(&IntentOp::Format {
                start_paragraph: p,
                start_offset: session.byte(p, 2),
                end_paragraph: p,
                end_offset: session.byte(p, 10),
                change: FormatChange::ClearFormatting,
            });
            let _ = (s, k);
            for op in ops
                .into_iter()
                .filter(|op| matches!(op, DocxOp::Format { .. }))
            {
                total.add(session.commit(&op));
            }
        }
        char_formats.push(total.div(FORMATS));
    }
    report("clear formatting (seq.)", char_formats);

    // Comments.
    const COMMENTS: u32 = 5;
    let mut comments = Vec::new();
    for _ in 0..samples {
        let mut total = Phases::default();
        for _ in 0..COMMENTS {
            let p = session.middle_paragraph();
            total.add(session.run(&IntentOp::AddComment {
                start_paragraph: p,
                start_offset: Some(session.byte(p, 1)),
                end_paragraph: p,
                end_offset: Some(session.byte(p, 6)),
                text: "A remark".to_string(),
                author: Some("bench".to_string()),
                initials: None,
                date: None,
            }));
        }
        comments.push(total.div(COMMENTS));
    }
    report("add a comment", comments);

    // Tracked typing: jade's revision session is switched on and off
    // around each insertion.
    session.commit(&DocxOp::SetTracking { on: true });
    const TRACKED: u32 = 10;
    let mut tracked = Vec::new();
    for _ in 0..samples {
        let p = session.middle_paragraph();
        let mut total = Phases::default();
        for k in 0..TRACKED as usize {
            total.add(session.run(&IntentOp::InsertText {
                paragraph: p,
                offset: session.byte(p, 3 + k),
                text: "t".to_string(),
            }));
        }
        tracked.push(total.div(TRACKED));
    }
    report("type (tracked)", tracked);
    session.commit(&DocxOp::SetTracking { on: false });

    // Marks: one bold mark per paragraph over its first characters, then
    // the cost of one more mark and of writing the file.
    for marks in [10usize, 100, 1000] {
        let mut session = Session::open(&bytes);
        let paragraphs = session.model.paragraphs();
        let targets: Vec<usize> = paragraphs
            .iter()
            .enumerate()
            .filter(|(_, p)| p.chars.len() >= 8)
            .map(|(n, _)| n)
            .collect();
        let mut mark_cost = Vec::new();
        for (k, p) in targets.iter().cycle().enumerate().take(marks) {
            let shift = (k / targets.len()) % 4;
            let phases = session.run(&IntentOp::Format {
                start_paragraph: *p,
                start_offset: session.byte(*p, shift),
                end_paragraph: *p,
                end_offset: session.byte(*p, shift + 4),
                change: FormatChange::Bold { value: k % 2 == 0 },
            });
            mark_cost.push(phases);
        }
        let tail = mark_cost.split_off(mark_cost.len().saturating_sub(samples));
        report(&format!("mark #{marks}"), tail);
        let mut exports = Vec::new();
        for _ in 0..samples {
            let started = Instant::now();
            let bytes = session.jade.export(&session.model).expect("exports");
            assert!(!bytes.is_empty());
            exports.push(Phases {
                materialize: started.elapsed(),
                ..Phases::default()
            });
        }
        report(&format!("export with {marks} marks"), exports);
    }
    let mut base = Vec::new();
    for _ in 0..samples {
        let started = Instant::now();
        session.jade.export_base().expect("exports");
        base.push(Phases {
            materialize: started.elapsed(),
            ..Phases::default()
        });
    }
    report("export base (no marks)", base);
}
