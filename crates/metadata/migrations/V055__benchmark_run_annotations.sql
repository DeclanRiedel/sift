ALTER TABLE benchmark_run ADD COLUMN annotation_revision INTEGER NOT NULL DEFAULT 0 CHECK(annotation_revision >= 0);
