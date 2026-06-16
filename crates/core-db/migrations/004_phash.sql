-- Perceptual hash (64-bit dHash) for near-duplicate detection. Nullable: computed lazily on the
-- first perceptual dedup scan (and for newly-imported rows on the next scan), not at index time.
-- Stored as a signed 64-bit INTEGER (the u64 dHash is reinterpreted bit-for-bit). No btree index —
-- Hamming-distance matching can't use one; the scan loads all phashes and compares in memory.
ALTER TABLE images ADD COLUMN phash INTEGER;
