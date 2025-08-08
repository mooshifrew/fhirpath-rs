//! Performance baseline measurement for Phase 0 optimizations

use octofhir_fhirpath::engine::FhirPathEngine;
use serde_json::Value;
use std::fs;
use std::time::Instant;

const EXPRESSIONS: &[(&str, &str)] = &[
    ("simple_bundle_traversal", "Bundle.entry"),
    ("bundle_resource_filter", "Bundle.entry.resource"),
    (
        "bundle_patient_names",
        "Bundle.entry.resource.where($this is Patient).name",
    ),
    (
        "complex_bundle_filter",
        "Bundle.entry.resource.where($this is Patient).name.where(use = 'official').given",
    ),
];

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🚀 FHIRPath Performance Baseline - Phase 0");
    println!("============================================");

    // Load test datasets
    let small = serde_json::from_str::<Value>(&fs::read_to_string("benches/fixtures/small.json")?)?;
    let medium =
        serde_json::from_str::<Value>(&fs::read_to_string("benches/fixtures/medium.json")?)?;
    let large = serde_json::from_str::<Value>(&fs::read_to_string("benches/fixtures/large.json")?)?;

    let datasets = [
        ("Small (822KB)", small),
        ("Medium (5MB)", medium),
        ("Large (17MB)", large),
    ];

    for (dataset_name, dataset) in &datasets {
        println!("\n📊 Dataset: {dataset_name}");
        println!("{:-<50}", "");

        for (expr_name, expression) in EXPRESSIONS {
            let mut engine = FhirPathEngine::new();

            // Warm up
            for _ in 0..3 {
                let _ = engine.evaluate(expression, dataset.clone()).await;
            }

            // Measure 10 iterations
            let start = Instant::now();
            let iterations = 10;

            for _ in 0..iterations {
                let result = engine.evaluate(expression, dataset.clone()).await;
                match result {
                    Ok(_) => {}
                    Err(e) => println!("  ❌ Error: {e}"),
                }
            }

            let elapsed = start.elapsed();
            let avg_ms = elapsed.as_millis() as f64 / iterations as f64;

            println!("  {expr_name} - {avg_ms:.2}ms/eval");
        }
    }

    // Memory cloning baseline
    println!("\n🧠 Memory Operation Baseline");
    println!("{:-<50}", "");

    for (dataset_name, dataset) in &datasets {
        let start = Instant::now();
        let iterations = 100;

        for _ in 0..iterations {
            let _cloned = dataset.clone();
        }

        let elapsed = start.elapsed();
        let avg_us = elapsed.as_micros() as f64 / iterations as f64;

        println!("  {dataset_name} clone - {avg_us:.2}μs/clone");
    }

    Ok(())
}
