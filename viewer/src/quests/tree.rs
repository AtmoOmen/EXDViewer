use std::{collections::HashSet, ops::Range};

pub struct Genre {
    pub name: String,
    pub quests: Vec<u32>,
}

pub struct Category {
    pub name: String,
    pub genres: Vec<Genre>,
}

pub struct Section {
    pub name: String,
    pub categories: Vec<Category>,
}

pub struct Group {
    pub depth: u8,
    pub label: String,
    /// The quests beneath this group, as a range into [`Outline::order`].
    pub quests: Range<u32>,
    /// Groups that hold quests rather than more groups: the genres, and the leftover bucket.
    pub leaf: bool,
}

/// The journal tree flattened into pre-order groups over one contiguous quest ordering, so a
/// group's matches are a prefix-sum lookup and drawing it is a single walk.
pub struct Outline {
    pub order: Vec<u32>,
    pub groups: Vec<Group>,
    /// The quests with no journal entry, which the tree only lists on request.
    pub uncategorized: Option<u32>,
}

pub enum Row {
    Group(u32),
    Quest { node: u32, depth: u8 },
}

impl Outline {
    pub fn build(sections: &[Section], uncategorized: &[u32]) -> Self {
        let mut order = Vec::new();
        let mut groups: Vec<Group> = Vec::new();
        for section in sections {
            let (section_at, section_start) = (groups.len(), order.len() as u32);
            groups.push(Group {
                depth: 0,
                label: section.name.clone(),
                quests: 0..0,
                leaf: false,
            });
            for category in &section.categories {
                let (category_at, category_start) = (groups.len(), order.len() as u32);
                groups.push(Group {
                    depth: 1,
                    label: category.name.clone(),
                    quests: 0..0,
                    leaf: false,
                });
                for genre in &category.genres {
                    let start = order.len() as u32;
                    order.extend_from_slice(&genre.quests);
                    groups.push(Group {
                        depth: 2,
                        label: genre.name.clone(),
                        quests: start..order.len() as u32,
                        leaf: true,
                    });
                }
                groups[category_at].quests = category_start..order.len() as u32;
            }
            groups[section_at].quests = section_start..order.len() as u32;
        }

        let uncategorized = (!uncategorized.is_empty()).then(|| {
            let start = order.len() as u32;
            order.extend_from_slice(uncategorized);
            groups.push(Group {
                depth: 0,
                label: "Uncategorized".to_string(),
                quests: start..order.len() as u32,
                leaf: true,
            });
            groups.len() as u32 - 1
        });

        Self {
            order,
            groups,
            uncategorized,
        }
    }

    /// The rows to draw, and how many quests each listed group still holds under the filter.
    pub fn rows(
        &self,
        expanded: &HashSet<u32>,
        matched: &[bool],
        show_uncategorized: bool,
        out: &mut Vec<(Row, u32)>,
    ) {
        let mut hits = Vec::with_capacity(self.order.len() + 1);
        hits.push(0u32);
        for node in &self.order {
            hits.push(hits.last().unwrap() + u32::from(matched[*node as usize]));
        }

        out.clear();
        let mut skip_below: Option<u8> = None;
        for (at, group) in self.groups.iter().enumerate() {
            let at = at as u32;
            if skip_below.is_some_and(|depth| group.depth > depth) {
                continue;
            }
            skip_below = None;
            if self.uncategorized == Some(at) && !show_uncategorized {
                continue;
            }
            let count = hits[group.quests.end as usize] - hits[group.quests.start as usize];
            if count == 0 {
                skip_below = Some(group.depth);
                continue;
            }
            out.push((Row::Group(at), count));
            if !expanded.contains(&at) {
                skip_below = Some(group.depth);
                continue;
            }
            if group.leaf {
                out.extend(
                    self.order[group.quests.start as usize..group.quests.end as usize]
                        .iter()
                        .filter(|node| matched[**node as usize])
                        .map(|node| {
                            (
                                Row::Quest {
                                    node: *node,
                                    depth: group.depth + 1,
                                },
                                0,
                            )
                        }),
                );
            }
        }
    }

    /// Every group on the path down to a quest, so selecting one can open the tree to it.
    pub fn path_to(&self, node: u32) -> Vec<u32> {
        let Some(at) = self.order.iter().position(|other| *other == node) else {
            return Vec::new();
        };
        let at = at as u32;
        self.groups
            .iter()
            .enumerate()
            .filter(|(_, group)| group.quests.contains(&at))
            .map(|(group, _)| group as u32)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Outline {
        Outline::build(
            &[Section {
                name: "Sidequests".into(),
                categories: vec![
                    Category {
                        name: "Weapon Enhancement".into(),
                        genres: vec![
                            Genre {
                                name: "Zodiac".into(),
                                quests: vec![0, 1],
                            },
                            Genre {
                                name: "Relic".into(),
                                quests: vec![2],
                            },
                        ],
                    },
                    Category {
                        name: "Housing".into(),
                        genres: vec![Genre {
                            name: "Estates".into(),
                            quests: vec![3],
                        }],
                    },
                ],
            }],
            &[4],
        )
    }

    fn labels(outline: &Outline, rows: &[(Row, u32)]) -> Vec<String> {
        rows.iter()
            .map(|(row, count)| match row {
                Row::Group(at) => format!("{} ({count})", outline.groups[*at as usize].label),
                Row::Quest { node, depth } => format!("q{node}@{depth}"),
            })
            .collect()
    }

    #[test]
    fn groups_span_exactly_their_children() {
        let outline = sample();
        assert_eq!(outline.order, [0, 1, 2, 3, 4]);
        assert_eq!(
            outline.groups[0].quests,
            0..4,
            "the section, minus the leftovers"
        );
        assert_eq!(outline.groups[1].quests, 0..3);
        assert_eq!(outline.groups[6].quests, 4..5);
        assert_eq!(outline.uncategorized, Some(6));
        assert_eq!(outline.path_to(2), [0, 1, 3]);
    }

    #[test]
    fn collapsed_groups_hide_their_subtrees() {
        let outline = sample();
        let matched = [true; 5];
        let mut rows = Vec::new();

        outline.rows(&HashSet::new(), &matched, false, &mut rows);
        assert_eq!(labels(&outline, &rows), ["Sidequests (4)"]);

        outline.rows(&HashSet::from([0, 6]), &matched, true, &mut rows);
        assert_eq!(
            labels(&outline, &rows),
            [
                "Sidequests (4)",
                "Weapon Enhancement (3)",
                "Housing (1)",
                "Uncategorized (1)",
                "q4@1"
            ]
        );
    }

    #[test]
    fn a_filter_drops_the_groups_it_empties() {
        let outline = sample();
        let mut matched = [false; 5];
        matched[3] = true;
        let mut rows = Vec::new();

        outline.rows(
            &HashSet::from([0, 1, 2, 3, 4, 5, 6]),
            &matched,
            true,
            &mut rows,
        );
        assert_eq!(
            labels(&outline, &rows),
            ["Sidequests (1)", "Housing (1)", "Estates (1)", "q3@3"],
            "the empty category and the leftover bucket are gone, not just their quests"
        );
    }
}
