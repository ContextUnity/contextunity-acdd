use acdd_core::{pack_knapsack_indices, KnapsackItem};

#[test]
fn knapsack_packs_relevance_within_the_byte_budget() {
    let items = [
        KnapsackItem {
            weight: 11,
            score: 50,
        },
        KnapsackItem {
            weight: 5,
            score: 4,
        },
        KnapsackItem {
            weight: 5,
            score: 5,
        },
    ];
    assert_eq!(pack_knapsack_indices(&items, 10), [1, 2]);
    let ties = [(1, 8), (3, 8), (4, 3), (3, 6), (2, 6), (1, 8)]
        .map(|(weight, score)| KnapsackItem { weight, score });
    assert_eq!(pack_knapsack_indices(&ties, 9), [0, 1, 3, 5]);
    let unequal_cardinality = [
        (2, 2),
        (4, 0),
        (1, 1),
        (1, 1),
        (1, 1),
        (4, 0),
        (4, 0),
        (4, 0),
    ]
    .map(|(weight, score)| KnapsackItem { weight, score });
    assert_eq!(pack_knapsack_indices(&unequal_cardinality, 3), [0, 2]);
    let many = vec![KnapsackItem { weight: 1, score: 1 }; 32_768];
    assert_eq!(pack_knapsack_indices(&many, 1), [0]);
    assert_eq!(pack_knapsack_indices(&many, 2), [0, 1]);
    assert_eq!(
        pack_knapsack_indices(&many, 16_384),
        (0..16_384).collect::<Vec<_>>()
    );
    assert_eq!(
        pack_knapsack_indices(&many, many.len()),
        (0..many.len()).collect::<Vec<_>>()
    );
    for encoding in 0..4_usize.pow(4) {
        let items: Vec<_> = (0..4)
            .map(|i| KnapsackItem {
                weight: (encoding >> (i * 2)) & 3,
                score: ((encoding + i * 7) % 5) as u64,
            })
            .collect();
        for capacity in 0..8 {
            let chosen = pack_knapsack_indices(&items, capacity);
            let weight: usize = chosen.iter().map(|&i| items[i].weight).sum();
            let score: u64 = chosen.iter().map(|&i| items[i].score).sum();
            let oracle = (0..1 << items.len())
                .filter_map(|mask| {
                    let indices: Vec<usize> = (0..items.len())
                        .filter(|&i| mask & (1 << i) != 0)
                        .collect();
                    let (w, s) = indices.iter().fold((0, 0), |(w, s), &i| {
                        (w + items[i].weight, s + items[i].score)
                    });
                    (w <= capacity && indices.iter().all(|&i| items[i].score > 0))
                        .then_some((s, indices))
                })
                .max_by(|(s1, idx1), (s2, idx2)| s1.cmp(s2).then_with(|| idx2.cmp(idx1)))
                .unwrap();
            assert!(weight <= capacity);
            assert_eq!(score, oracle.0, "{items:?}, capacity {capacity}");
            assert_eq!(chosen, oracle.1, "{items:?}, capacity {capacity}");
            assert!(chosen.windows(2).all(|pair| pair[0] < pair[1]));
        }
    }
}
