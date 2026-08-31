pub mod engine;
pub mod mock_env;
pub mod workspace;

// Runner CLI / test harness
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_benchmark_engine() {
        let base_dir = std::env::temp_dir().join("kerux_bench_test");
        if base_dir.exists() {
            std::fs::remove_dir_all(&base_dir).unwrap();
        }

        let workspace = workspace::SyntheticWorkspace::generate_trap_workspace(&base_dir).unwrap();
        let mut engine = engine::BenchmarkEngine::new(workspace);

        let rubric = engine.run_evaluation();

        assert!(rubric.execution_success);
        assert!((rubric.tool_calling_accuracy - (1.0 / 3.0)).abs() < 1e-5); // 1 success out of 3 calls

        // Clean up
        if base_dir.exists() {
            std::fs::remove_dir_all(&base_dir).unwrap();
        }
    }
}
