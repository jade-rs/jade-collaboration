//! Pictures: inserted with their image bytes in the operation (base64 on
//! the wire, at most `MAX_PICTURE_BYTES`), deleted like any shape, moved
//! through the geometry registers. A picture keeps its aspect ratio
//! locked, as jade writes it, so it is resized by a sequenced
//! `ResizeShape`, which jade's ratio check needs to see in log order.

use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_sync::{ApplyContext, Document, Lane};
use jade_pptx_collab::{
    Geometry, GeometryEdit, Image, PptxEffect, PptxModel, PptxOp, ShapeKind, ShapeRef,
    MAX_PICTURE_BYTES,
};

/// A 1×1 PNG.
pub const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0x64, 0x60, 0xf8, 0x5f,
    0x0f, 0x00, 0x02, 0x87, 0x01, 0x80, 0xeb, 0x47, 0xba, 0x92, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

fn at(origin: u64, base_seq: u64, seq: Option<u64>) -> ApplyContext {
    ApplyContext {
        origin: ReplicaId(origin),
        base_seq,
        seq,
    }
}

fn id(n: u64, replica: u64) -> Id {
    Id::new(1000 + n, ReplicaId(replica))
}

fn deck() -> PptxModel {
    PptxModel::from_texts(&[&["Title", "Body"], &["Second"]])
}

fn frame(width: i64, height: i64) -> Geometry {
    Geometry {
        x: 100,
        y: 200,
        width,
        height,
        rotation: 0,
    }
}

fn insert(slide: Id, picture: Id, geometry: Geometry, image: &[u8]) -> PptxOp {
    PptxOp::InsertPicture {
        slide,
        id: picture,
        geometry,
        image: Image(image.to_vec()),
    }
}

#[test]
fn a_picture_is_a_sequenced_shape_on_top_numbered_by_the_model() {
    let mut model = deck();
    let slide = model.slides()[0].id;
    let op = insert(slide, id(1, 1), frame(4000, 3000), PNG);
    assert_eq!(PptxModel::lane(&op), Lane::Sequenced);
    let effect = model.apply(&op, &at(1, 0, Some(1))).unwrap();
    assert_eq!(
        effect,
        vec![PptxEffect::InsertPicture {
            slide: 0,
            shape: ShapeRef {
                slide,
                shape: id(1, 1),
                number: 4,
            },
            geometry: frame(4000, 3000),
            image: Image(PNG.to_vec()),
        }]
    );
    let shape = model.slides()[0].shapes.last().unwrap().clone();
    assert_eq!(shape.id, id(1, 1));
    assert_eq!(shape.kind, ShapeKind::Picture);
    assert_eq!(shape.text, None);
    assert_eq!(shape.geometry, Some(frame(4000, 3000)));
    assert!(shape.deletable && shape.aspect_locked);
    assert!(model.max_lamport() >= 1001);
}

#[test]
fn pictures_jade_cannot_take_are_refused() {
    let mut model = deck();
    let slide = model.slides()[0].id;
    let before = model.clone();
    let mut huge = PNG.to_vec();
    huge.resize(MAX_PICTURE_BYTES + 1, 0);
    let refused = [
        insert(slide, id(1, 1), frame(10, 10), b"GIF89a not supported"),
        insert(slide, id(1, 1), frame(10, 10), &huge),
        insert(slide, id(1, 1), frame(0, 10), PNG),
        insert(slide, id(1, 1), frame(10, -1), PNG),
        insert(id(99, 9), id(1, 1), frame(10, 10), PNG),
    ];
    for op in &refused {
        assert!(model.apply(op, &at(1, 0, Some(1))).is_err(), "{op:?}");
    }
    assert_eq!(model, before);
    model
        .apply(
            &insert(slide, id(1, 1), frame(10, 10), PNG),
            &at(1, 0, Some(1)),
        )
        .unwrap();
    assert!(
        model
            .apply(
                &insert(slide, id(1, 1), frame(10, 10), PNG),
                &at(1, 0, Some(2))
            )
            .is_err(),
        "the id is taken"
    );
    // A JPEG's signature is accepted too.
    let jpeg = [0xff, 0xd8, 0xff, 0xe0, 0, 0x10];
    model
        .apply(
            &insert(slide, id(2, 1), frame(10, 10), &jpeg),
            &at(1, 0, Some(3)),
        )
        .unwrap();
}

/// Moving and rotating go through the convergent registers; the size of a
/// shape whose ratio is locked does not (a write to one side would break
/// it), but a sequenced resize that keeps the ratio — by jade's own
/// tolerance, against the current size — does.
#[test]
fn pictures_move_freely_and_resize_keeping_their_ratio() {
    let mut model = deck();
    let slide = model.slides()[0].id;
    let picture = id(1, 1);
    model
        .apply(
            &insert(slide, picture, frame(4000, 3000), PNG),
            &at(1, 0, Some(1)),
        )
        .unwrap();
    let mv = PptxOp::SetGeometry {
        slide,
        shape: picture,
        stamp: id(2, 1),
        edit: GeometryEdit {
            x: Some(7),
            rotation: Some(60_000),
            ..GeometryEdit::default()
        },
    };
    model.apply(&mv, &at(1, 1, None)).unwrap();
    let stretch = PptxOp::SetGeometry {
        slide,
        shape: picture,
        stamp: id(3, 1),
        edit: GeometryEdit {
            width: Some(8000),
            ..GeometryEdit::default()
        },
    };
    assert!(model.apply(&stretch, &at(1, 1, None)).is_err());

    let resize = |stamp, width, height| PptxOp::ResizeShape {
        slide,
        shape: picture,
        stamp,
        width,
        height,
    };
    assert_eq!(PptxModel::lane(&resize(id(4, 1), 1, 1)), Lane::Sequenced);
    let effect = model
        .apply(&resize(id(4, 1), 8000, 6000), &at(1, 1, Some(2)))
        .unwrap();
    let geometry = Geometry {
        x: 7,
        y: 200,
        width: 8000,
        height: 6000,
        rotation: 60_000,
    };
    assert_eq!(
        effect,
        vec![PptxEffect::SetTransform {
            slide: 0,
            shape: ShapeRef {
                slide,
                shape: picture,
                number: 4
            },
            geometry
        }]
    );
    // Within jade's tolerance (|w·H − h·W| ≤ W + H): one EMU off is fine.
    model
        .apply(&resize(id(5, 1), 4001, 3000), &at(1, 2, Some(3)))
        .unwrap();
    // Not so a different ratio, or a size jade cannot write.
    assert!(model
        .apply(&resize(id(6, 1), 4000, 4000), &at(1, 3, Some(4)))
        .is_err());
    assert!(model
        .apply(&resize(id(6, 1), -4, -3), &at(1, 3, Some(4)))
        .is_err());
    // A shape without a locked ratio can be resized either way.
    let text = model.slides()[0].shapes[0].id;
    model
        .apply(
            &PptxOp::ResizeShape {
                slide,
                shape: text,
                stamp: id(7, 1),
                width: 5,
                height: 900,
            },
            &at(1, 3, Some(4)),
        )
        .unwrap();
    let g = model.slides()[0].shapes[0].geometry.unwrap();
    assert_eq!((g.width, g.height), (5, 900));
}

#[test]
fn pictures_are_deleted_and_duplicated_like_other_shapes() {
    let mut model = deck();
    let slide = model.slides()[0].id;
    model
        .apply(
            &insert(slide, id(1, 1), frame(40, 30), PNG),
            &at(1, 0, Some(1)),
        )
        .unwrap();
    model
        .apply(
            &PptxOp::DuplicateSlide {
                slide,
                id: id(2, 1),
            },
            &at(1, 1, Some(2)),
        )
        .unwrap();
    let copy = model.slides()[1].shapes.last().unwrap().clone();
    assert_eq!((copy.id, copy.kind), (id(1, 1), ShapeKind::Picture));
    model
        .apply(
            &PptxOp::DeleteShape {
                slide,
                shape: id(1, 1),
            },
            &at(1, 2, Some(3)),
        )
        .unwrap();
    assert!(model.slides()[0]
        .shapes
        .iter()
        .all(|s| s.kind != ShapeKind::Picture));
    assert_eq!(model.slides()[1].shapes.len(), 3);
}

/// Image bytes travel as base64 inside the operation's JSON.
#[test]
fn the_image_travels_as_base64() {
    let op = insert(id(0, 0), id(1, 1), frame(1, 1), PNG);
    let json = serde_json::to_value(&op).unwrap();
    assert_eq!(
        json["image"],
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg=="
    );
    assert_eq!(serde_json::from_value::<PptxOp>(json).unwrap(), op);
    let bad = serde_json::json!({
        "type": "insert_picture",
        "slide": {"lamport": 1, "replica": 0},
        "id": {"lamport": 2, "replica": 1},
        "geometry": {"x": 0, "y": 0, "width": 1, "height": 1, "rotation": 0},
        "image": "not base64!",
    });
    assert!(serde_json::from_value::<PptxOp>(bad).is_err());
}
