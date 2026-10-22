//! Real native writers behind a deterministic before/after-write failure.
use super::*;
use jade_collab_sync::ApplyContext;
use std::cell::Cell;
use std::marker::PhantomData;

#[derive(Clone, Copy)]
struct Settings {
    at: usize,
    after: bool,
}
thread_local! {static SETTINGS:Cell<Settings>=const {Cell::new(Settings {at:usize::MAX,after:false})};}
struct Faulting<H>(PhantomData<H>);
struct File<M> {
    inner: M,
    calls: usize,
    settings: Settings,
}
impl<M> File<M> {
    fn wrap(inner: M) -> Self {
        Self {
            inner,
            calls: 0,
            settings: SETTINGS.with(Cell::get),
        }
    }
}
impl<D: Document, M: Materializer<D>> Materializer<D> for File<M> {
    fn may_derive(&self, doc: &D, op: &D::Op) -> bool {
        self.inner.may_derive(doc, op)
    }
    fn fork(&self, doc: &D) -> Result<Self, String> {
        Ok(Self {
            inner: self.inner.fork(doc)?,
            calls: self.calls,
            settings: self.settings,
        })
    }
    fn admit(&self, doc: &D, op: &D::Op) -> Result<(), Rejection> {
        self.inner.admit(doc, op)
    }
    fn derived(&mut self, doc: &D, op: &D::Op) -> Result<Vec<D::Op>, Rejection> {
        self.inner.derived(doc, op)
    }
    fn materialize(
        &mut self,
        doc: &D,
        effect: D::Effect,
        ctx: &ApplyContext,
    ) -> Result<(), String> {
        self.calls += 1;
        let failing = self.calls == self.settings.at;
        if failing && !self.settings.after {
            return Err("injected native failure before writing".into());
        }
        self.inner.materialize(doc, effect, ctx)?;
        if failing {
            return Err("injected native failure after writing".into());
        }
        Ok(())
    }
}
impl<H: Hosted> Hosted for Faulting<H> {
    type Doc = H::Doc;
    type Op = H::Op;
    type Mat = File<H::Mat>;
    const FORMAT: Format = H::FORMAT;
    fn load(bytes: &[u8], model: &Self::Doc) -> Result<Self::Mat, String> {
        H::load(bytes, model).map(File::wrap)
    }
    fn open(bytes: &[u8]) -> Result<(Self::Doc, Self::Mat), String> {
        H::open(bytes).map(|(doc, mat)| (doc, File::wrap(mat)))
    }
    fn export(doc: &Self::Doc, mat: &Self::Mat) -> Result<Vec<u8>, String> {
        H::export(doc, &mat.inner)
    }
    fn checkpoint_file(mat: &Self::Mat) -> Result<Vec<u8>, String> {
        H::checkpoint_file(&mat.inner)
    }
    fn inspect(doc: &Self::Doc, mat: &Self::Mat) -> Result<Value, String> {
        H::inspect(doc, &mat.inner)
    }
    fn author_op(
        doc: &Self::Doc,
        replica: ReplicaId,
        author: &str,
        op: &Self::Op,
    ) -> Option<Self::Op> {
        H::author_op(doc, replica, author, op)
    }
}
fn spawn_inner<H: Hosted>(source: Source, at: usize, after: bool) -> Result<Actor, String> {
    let (ready, answer) = mpsc::channel();
    let actor = Actor::test_worker(
        mailbox::DEFAULT_INCOMING_LIMIT,
        mailbox::DEFAULT_INCOMING_BYTES,
        move |inbox| {
            SETTINGS.set(Settings { at, after });
            let mut hosting = match Hosting::<Faulting<H>>::open(source) {
                Ok(hosting) => {
                    let _ = ready.send(Ok(()));
                    hosting
                }
                Err(error) => {
                    let _ = ready.send(Err(error));
                    return;
                }
            };
            if let Err(error) = hosting.serve(inbox) {
                eprintln!("fault-injected native room stopped: {error}");
            }
        },
    );
    answer.recv().map_err(|e| e.to_string())??;
    Ok(actor)
}
pub(crate) fn spawn(
    format: Format,
    source: Source,
    at: usize,
    after: bool,
) -> Result<Actor, String> {
    match format {
        Format::Docx => spawn_inner::<Docx>(source, at, after),
        Format::Xlsx => spawn_inner::<Xlsx>(source, at, after),
        Format::Pptx => spawn_inner::<Pptx>(source, at, after),
    }
}
