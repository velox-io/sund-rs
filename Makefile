.PHONY: bench bench-base bench-parse bench-compare bench-all-payloads clean

NDEC_DIR ?= ../velox-json-work/native/ndec
PAYLOAD  ?= $(NDEC_DIR)/test/data/bench_payload.json

# Default: run both base and parse
bench: bench-release
	@./target/release/sund_bench --payload=$(PAYLOAD)

bench-base: bench-release
	@./target/release/sund_bench base --payload=$(PAYLOAD)

bench-parse: bench-release
	@./target/release/sund_bench parse --payload=$(PAYLOAD)

bench-release:
	@cargo build --release -p sund-bench 2>&1 | tail -1

# Run all payload shapes for comparison
bench-all-payloads: bench-release
	@echo "================================================================"
	@echo "  sund benchmark — all payload shapes"
	@echo "================================================================"
	@for f in \
		$(NDEC_DIR)/test/data/bench_payload_longstr.json \
		$(NDEC_DIR)/test/data/twitter.json \
		$(NDEC_DIR)/test/data/bench_payload_32k.json \
		$(NDEC_DIR)/test/data/twitter-compact.json \
		$(NDEC_DIR)/test/data/bench_payload_numbers.json \
		$(NDEC_DIR)/test/data/bench_payload.json \
	; do \
		if [ -f "$$f" ]; then \
			echo ""; \
			echo "--- $$(basename $$f) ($$(wc -c < $$f | tr -d ' ') bytes) ---"; \
			./target/release/sund_bench --payload=$$f 2>/dev/null; \
		fi; \
	done

# Head-to-head comparison with ndec (C)
bench-compare:
	@./benches/bench_compare.sh $(PAYLOAD)

clean:
	cargo clean
