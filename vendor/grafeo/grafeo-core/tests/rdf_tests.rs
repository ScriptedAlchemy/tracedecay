//! Integration tests for RDF support in grafeo-core.

#[cfg(feature = "triple-store")]
mod statistics_tests {
    use grafeo_core::statistics::{RdfStatistics, RdfStatisticsCollector};

    #[test]
    fn test_empty_statistics() {
        let stats = RdfStatistics::new();
        assert_eq!(stats.total_triples, 0);
        assert_eq!(stats.subject_count, 0);
    }

    #[test]
    fn test_statistics_collector_basic() {
        let mut collector = RdfStatisticsCollector::new();

        collector.record_triple("s1", "p1", "o1");
        collector.record_triple("s1", "p1", "o2");
        collector.record_triple("s2", "p1", "o1");
        collector.record_triple("s1", "p2", "o3");

        let stats = collector.build();
        assert_eq!(stats.total_triples, 4);
        assert_eq!(stats.subject_count, 2); // s1, s2
        assert_eq!(stats.predicate_count, 2); // p1, p2
    }

    #[test]
    fn test_statistics_collector_larger_dataset() {
        let mut collector = RdfStatisticsCollector::new();

        // Create a dataset with many triples
        for i in 0..100 {
            collector.record_triple(&format!("s{}", i % 10), "type", "Person");
            collector.record_triple(&format!("s{}", i % 10), "name", &format!("Name{}", i));
        }

        let stats = collector.build();
        assert_eq!(stats.total_triples, 200);
        assert_eq!(stats.subject_count, 10);
    }

    #[test]
    fn test_cardinality_estimation() {
        let mut collector = RdfStatisticsCollector::new();

        for i in 0..100 {
            collector.record_triple(&format!("s{}", i % 10), "type", "Person");
            collector.record_triple(&format!("s{}", i % 10), "name", &format!("Name{}", i));
        }

        let stats = collector.build();

        // Test cardinality estimation for triple patterns
        let card = stats.estimate_triple_pattern_cardinality(true, None, false);
        assert!(card > 0.0);
    }

    #[test]
    fn test_predicate_statistics() {
        let mut collector = RdfStatisticsCollector::new();

        collector.record_triple("s1", "knows", "s2");
        collector.record_triple("s1", "knows", "s3");
        collector.record_triple("s2", "knows", "s3");

        let stats = collector.build();

        if let Some(pred_stats) = stats.predicates.get("knows") {
            assert_eq!(pred_stats.triple_count, 3);
        }
    }

    #[test]
    fn test_distinct_counts() {
        let mut collector = RdfStatisticsCollector::new();

        // Add triples with repeated subjects and objects
        collector.record_triple("alix", "knows", "gus");
        collector.record_triple("alix", "knows", "vincent");
        collector.record_triple("gus", "knows", "vincent");
        collector.record_triple("gus", "knows", "alix");

        let stats = collector.build();

        // Should have 2 distinct subjects
        assert_eq!(stats.subject_count, 2); // alix, gus
        // Should have 3 distinct objects
        assert_eq!(stats.object_count, 3); // gus, vincent, alix
    }
}

#[cfg(all(feature = "triple-store", feature = "parallel"))]
mod parallel_tests {
    use grafeo_common::types::Value;
    use grafeo_core::execution::parallel::{Morsel, ParallelSource, ParallelTripleScanSource};
    use grafeo_core::execution::pipeline::Source;

    #[test]
    fn test_parallel_triple_source_creation() {
        let triples = vec![
            (
                Value::String("s1".into()),
                Value::String("p1".into()),
                Value::String("o1".into()),
            ),
            (
                Value::String("s2".into()),
                Value::String("p2".into()),
                Value::String("o2".into()),
            ),
        ];

        let source = ParallelTripleScanSource::new(triples);

        assert_eq!(source.total_rows(), Some(2));
        assert!(source.is_partitionable());
        assert_eq!(source.num_columns(), 3);
    }

    #[test]
    fn test_parallel_triple_source_partitioning() {
        let triples: Vec<(Value, Value, Value)> = (0..100)
            .map(|i| {
                (
                    Value::String(format!("s{}", i).into()),
                    Value::String(format!("p{}", i).into()),
                    Value::String(format!("o{}", i).into()),
                )
            })
            .collect();

        let source = ParallelTripleScanSource::new(triples);

        let morsels = source.generate_morsels(25, 0);
        assert_eq!(morsels.len(), 4); // 100 / 25 = 4 morsels
    }

    #[test]
    fn test_parallel_triple_source_read() {
        let triples = vec![
            (
                Value::String("alix".into()),
                Value::String("knows".into()),
                Value::String("gus".into()),
            ),
            (
                Value::String("gus".into()),
                Value::String("knows".into()),
                Value::String("vincent".into()),
            ),
        ];

        let mut source = ParallelTripleScanSource::new(triples);

        let chunk = source.next_chunk(10).unwrap().unwrap();
        assert_eq!(chunk.len(), 2);
        assert_eq!(chunk.num_columns(), 3);
    }

    #[test]
    fn test_parallel_triple_source_partition_read() {
        let triples: Vec<(Value, Value, Value)> = (0..100)
            .map(|i| {
                (
                    Value::String(format!("s{}", i).into()),
                    Value::String(format!("p{}", i).into()),
                    Value::String(format!("o{}", i).into()),
                )
            })
            .collect();

        let source = ParallelTripleScanSource::new(triples);

        // Create a morsel for rows 20-50
        let morsel = Morsel::new(0, 0, 20, 50);
        let mut partition = source.create_partition(&morsel);

        let mut total = 0;
        while let Ok(Some(chunk)) = partition.next_chunk(10) {
            total += chunk.len();
        }
        assert_eq!(total, 30); // rows 20-50 = 30 rows
    }

    #[test]
    fn test_parallel_triple_source_reset() {
        let triples = vec![(
            Value::String("s1".into()),
            Value::String("p1".into()),
            Value::String("o1".into()),
        )];

        let mut source = ParallelTripleScanSource::new(triples);

        // Read all data
        let _ = source.next_chunk(10).unwrap();
        assert!(source.next_chunk(10).unwrap().is_none());

        // Reset and read again
        source.reset();
        let chunk = source.next_chunk(10).unwrap();
        assert!(chunk.is_some());
    }
}
