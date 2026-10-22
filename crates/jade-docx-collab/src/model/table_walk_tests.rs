use super::*;
use proptest::prelude::*;

fn id(n: usize) -> Id {
    Id::new(n as u64, ReplicaId(1))
}

#[test]
fn sparse_walk_visits_two_stored_cells_of_a_hundred_million_position_grid() {
    let rows: Vec<_> = (1..=10000).map(id).collect();
    let columns: Vec<_> = (20000..30000).map(id).collect();
    let mut table = Table::new(&[], &[], false, true);
    table.grid = Grid::new(rows.clone(), columns.clone());
    table.add_cell(rows[0], columns[9999], Story::empty_paragraph());
    table.add_cell(rows[9999], columns[0], Story::empty_paragraph());
    let cells = table.visible_cells(&rows, &columns);
    assert_eq!(
        cells.iter().map(|(r, c, _)| (*r, *c)).collect::<Vec<_>>(),
        vec![(0, 9999), (9999, 0)]
    );
}

proptest! {
    #[test]
    fn sparse_walk_matches_row_major_reference_through_reordered_ids_and_tombstones(
        nrows in 1usize..12,ncols in 1usize..12,
        steps in prop::collection::vec((any::<usize>(),any::<usize>(),any::<bool>()),0..70),
    ) {
        let all_rows:Vec<_>=(1..=nrows).rev().map(id).collect();
        let all_columns:Vec<_>=(100..100+ncols).rev().map(id).collect();
        let mut table=Table::new(&[],&[],false,true);
        table.grid=Grid::new(all_rows.clone(),all_columns.clone());
        for (r,c,deleted) in steps {
            let (row,column)=(all_rows[r%nrows],all_columns[c%ncols]);
            table.add_cell(row,column,Story::empty_paragraph());
            if deleted {let _=table.grid.apply(&GridOp::DeleteRow {id:row});}
            else if c%3==0 {let _=table.grid.apply(&GridOp::DeleteColumn {id:column});}
            let rows=table.grid.rows();let columns=table.grid.columns();
            let mut reference=Vec::new();
            for (r,row) in rows.iter().enumerate() {for (c,column) in columns.iter().enumerate() {
                if let Some(shown)=table.layout.get(&(*row,*column)) {reference.push((r,c,shown.as_slice()));}
            }}
            prop_assert_eq!(table.visible_cells(&rows,&columns),reference);
        }
    }
}
