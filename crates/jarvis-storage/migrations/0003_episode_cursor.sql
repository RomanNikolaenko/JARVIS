-- Stable pagination when multiple episodes share the same millisecond.
CREATE INDEX episodes_by_cursor ON episodes (occurred_at DESC, uid DESC);
DROP INDEX episodes_by_time;
