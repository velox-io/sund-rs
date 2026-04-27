.PHONY: bench clean

bench:
	@RUSTFLAGS="-C target-cpu=native" cargo build --release -p sund-bench
	for f in $(BENCH_DATA_DIR)/$(if $(BENCH_FILTER),$(BENCH_FILTER),*.json); do \
		./target/release/sund_bench --payload=$$f; \
	done

clean:
	cargo clean
