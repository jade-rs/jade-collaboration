//! Building the model from a deck, with jade reading it.

use std::collections::BTreeMap;

use jade_collab_core::id::{Id, ReplicaId};
use jade_pptx_reader::{Deck, ShapeItem};
use jade_pptx_writer::{EditSession, ShapeTransformInfo};

use crate::model::{
    flow_from_text, Geometry, Locks, Placement, PptxModel, Shape, ShapeKind, Slide, Story, Template,
};

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("jade cannot read the deck: {0}")]
    Read(String),
}

fn read(error: impl std::fmt::Display) -> LoadError {
    LoadError::Read(error.to_string())
}

/// One shape of a slide's shape tree as the model and the materializer both
/// enumerate it: depth first, shapes without an id skipped (nothing can
/// address them), groups entered either way.
pub(crate) struct TreeShape {
    pub(crate) id: u32,
    pub(crate) kind: ShapeKind,
    pub(crate) top_level: bool,
    /// The index (in this list) of the enclosing group.
    pub(crate) group: Option<usize>,
}

pub(crate) fn shape_tree(shapes: &[ShapeItem]) -> Vec<TreeShape> {
    fn walk(items: &[ShapeItem], group: Option<usize>, top: bool, out: &mut Vec<TreeShape>) {
        for item in items {
            let (id, kind) = match item {
                ShapeItem::Shape(s) => (s.id, ShapeKind::Shape),
                ShapeItem::Picture(s) => (s.id, ShapeKind::Picture),
                ShapeItem::Group(s) => (s.id, ShapeKind::Group),
                ShapeItem::Table(s) => (s.id, ShapeKind::Table),
                ShapeItem::Chart(s) => (s.id, ShapeKind::Chart),
            };
            let mut inner = group;
            if let Some(id) = id {
                out.push(TreeShape {
                    id,
                    kind,
                    top_level: top,
                    group,
                });
                if kind == ShapeKind::Group {
                    inner = Some(out.len() - 1);
                }
            }
            if let ShapeItem::Group(g) = item {
                walk(&g.children, inner, false, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(shapes, None, true, &mut out);
    out
}

pub(crate) fn geometry_of(info: &ShapeTransformInfo) -> Option<Geometry> {
    info.transform.map(|t| Geometry {
        x: t.offset_emu.0,
        y: t.offset_emu.1,
        width: t.extent_emu.0,
        height: t.extent_emu.1,
        rotation: i64::from(t.rotation_60000ths),
    })
}

struct Loader<'a> {
    bytes: &'a [u8],
    deck: Deck,
    next: u64,
}

impl Loader<'_> {
    fn id(&mut self) -> Id {
        let id = Id::new(self.next, ReplicaId::ORIGIN);
        self.next += 1;
        id
    }

    /// The shapes and notes of slide `index`, read through jade's writer —
    /// the component that will edit them — so the model holds exactly the
    /// text bodies and placements jade can change.
    fn slide(&mut self, index: usize) -> Result<(Vec<(Id, Shape)>, Story), LoadError> {
        let bytes = self.bytes;
        let infos: BTreeMap<u32, ShapeTransformInfo> =
            jade_pptx_writer::editable_shapes(bytes, index)
                .map_err(read)?
                .into_iter()
                .map(|info| (info.id, info))
                .collect();
        let tree = shape_tree(&self.deck.slides[index].content.shapes);
        let stamp = self.id();
        let keys: Vec<Id> = (1..=tree.len() as u64)
            .map(|k| Id::new(k, ReplicaId::ORIGIN))
            .collect();
        let mut shapes = Vec::new();
        for (k, entry) in tree.iter().enumerate() {
            let text = match entry.kind {
                ShapeKind::Shape => jade_pptx_writer::shape_text(bytes, index, entry.id)
                    .ok()
                    .map(|t| Story::new(flow_from_text(&t.text, &mut self.next), t.can_edit)),
                _ => None,
            };
            let info = infos.get(&entry.id).filter(|_| entry.top_level);
            let placement = info.filter(|i| i.can_select).and_then(|info| {
                Some(Placement::new(
                    stamp,
                    geometry_of(info)?,
                    Locks {
                        can_move: info.can_move,
                        // A write to one side would break a locked ratio.
                        can_resize: info.can_resize && !info.lock_aspect_ratio,
                        can_rotate: info.can_rotate,
                        group: info.kind == "group",
                    },
                ))
            });
            shapes.push((
                keys[k],
                Shape {
                    number: entry.id,
                    kind: entry.kind,
                    group: entry.group.map(|g| keys[g]),
                    text,
                    placement,
                    deletable: info.is_some_and(|i| i.can_delete),
                    deleted: false,
                },
            ));
        }
        let notes = match jade_pptx_writer::notes_text(bytes, index) {
            Ok(t) => Story::new(flow_from_text(&t.text, &mut self.next), t.can_edit),
            Err(_) => Story::new(
                flow_from_text(
                    self.deck.slides[index].notes.as_deref().unwrap_or_default(),
                    &mut self.next,
                ),
                false,
            ),
        };
        Ok((shapes, notes))
    }
}

/// The slide layout part of each slide, in order.
fn layouts(bytes: &[u8], deck: &Deck) -> Result<Vec<Option<String>>, LoadError> {
    let package = jade_opc::OpcPackage::open(bytes).map_err(read)?;
    deck.slides
        .iter()
        .map(|slide| {
            let rels = package
                .part_str(&jade_opc::rels_part_name(&slide.part_name))
                .map_err(read)?;
            Ok(match rels {
                None => None,
                Some(xml) => jade_opc::parse_relationships(&xml)
                    .map_err(read)?
                    .by_type_suffix("/slideLayout")
                    .into_iter()
                    .find(|r| !r.external)
                    .map(|r| jade_opc::resolve_target(&slide.part_name, &r.target)),
            })
        })
        .collect()
}

impl PptxModel {
    /// The model of a deck, with ids owned by [`ReplicaId::ORIGIN`].
    ///
    /// What a slide inserted later starts with is jade's business: for each
    /// layout in use, jade inserts a slide into a scratch copy of the deck
    /// and the model records what it put there.
    pub fn from_pptx(bytes: &[u8]) -> Result<Self, LoadError> {
        let deck = jade_pptx_reader::read_deck(bytes).map_err(read)?;
        let layouts = layouts(bytes, &deck)?;
        let mut loader = Loader {
            bytes,
            deck,
            next: 1,
        };
        let mut model = PptxModel {
            slides: BTreeMap::new(),
            order: Vec::new(),
            templates: Vec::new(),
            max_lamport: 0,
        };
        for (index, layout) in layouts.iter().enumerate() {
            let (shapes, notes) = loader.slide(index)?;
            let id = loader.id();
            let next_number = shapes.iter().map(|(_, s)| s.number).max().unwrap_or(1) + 1;
            model.order.push(id);
            model.slides.insert(
                id,
                Slide {
                    shapes,
                    notes,
                    layout: layout.clone(),
                    next_number,
                    copies: Vec::new(),
                    deleted: false,
                },
            );
        }

        // One template per layout in use (jade's fallback when the deck is
        // empty), each read from a slide jade inserts in front of a slide
        // with that layout.
        let mut wanted: Vec<(Option<String>, usize)> = Vec::new();
        for (index, layout) in layouts.iter().enumerate() {
            if !wanted.iter().any(|(l, _)| l == layout) {
                wanted.push((layout.clone(), index));
            }
        }
        if wanted.is_empty() {
            wanted.push((None, 0));
        }
        for (layout, index) in wanted {
            let mut scratch = EditSession::open(bytes).map_err(read)?;
            scratch.insert_slide(index).map_err(read)?;
            let scratch_bytes = scratch.bytes().to_vec();
            let mut template_loader = Loader {
                bytes: &scratch_bytes,
                deck: jade_pptx_reader::read_deck(&scratch_bytes).map_err(read)?,
                next: loader.next,
            };
            let (shapes, _) = template_loader.slide(index)?;
            loader.next = template_loader.next;
            model.templates.push(Template { layout, shapes });
        }
        model.max_lamport = loader.next - 1;
        Ok(model)
    }
}
