.PHONY: run-aft-search run-codegraph-replication run-codegraph-vs-aft-retrieval run-codegraph-vs-aft-agent bench-aft-search bench-codegraph-replication bench-codegraph-vs-aft-retrieval bench-codegraph-vs-aft-agent

run-aft-search bench-aft-search:
	docker compose -f benchmarks/aft-search/docker-compose.yml run --rm aft-search

run-codegraph-replication bench-codegraph-replication:
	docker compose -f benchmarks/codegraph-replication/docker-compose.yml run --rm codegraph-replication

run-codegraph-vs-aft-retrieval bench-codegraph-vs-aft-retrieval:
	docker compose -f benchmarks/codegraph-vs-aft-retrieval/docker-compose.yml run --rm aft
	docker compose -f benchmarks/codegraph-vs-aft-retrieval/docker-compose.yml run --rm codegraph

run-codegraph-vs-aft-agent bench-codegraph-vs-aft-agent:
	docker compose -f benchmarks/codegraph-vs-aft-agent/docker-compose.yml run --rm agent
