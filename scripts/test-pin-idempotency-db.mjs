import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';

// Execute the handler's exact SQL against the real migrated table, rolling back fixtures.
const url = process.env.PRIVCHAT_TEST_DATABASE_URL ?? process.env.DATABASE_URL;
assert.ok(url, 'Set PRIVCHAT_TEST_DATABASE_URL or DATABASE_URL; this test never skips.');
const insert = readFileSync(new URL('../src/rpc/message/pin_insert.sql', import.meta.url), 'utf8');
const sql = `
BEGIN;
SELECT pg_advisory_xact_lock(993076001);
DO $$ BEGIN
  IF EXISTS (SELECT 1 FROM privchat_group_pinned_messages WHERE group_id = -993076001) THEN
    RAISE EXCEPTION 'Fixture ID already in use';
  END IF;
END $$;
PREPARE pin(bigint, bigint, bigint, bigint, bigint) AS ${insert};
EXECUTE pin(-993076001, 1, -993076001, 101, 1000);
EXECUTE pin(-993076001, 1, -993076001, 102, 2000);
EXECUTE pin(-993076001, 2, -993076001, 102, 3000);
EXECUTE pin(-993076001, 1, -993076001, 103, 4000);
SELECT message_id, pinned_by, pinned_at FROM privchat_group_pinned_messages
 WHERE group_id = -993076001 ORDER BY pinned_at DESC;
SELECT count(*) FROM privchat_group_pinned_messages WHERE group_id = -993076001;
DELETE FROM privchat_group_pinned_messages WHERE group_id = -993076001 AND message_id = 1;
DELETE FROM privchat_group_pinned_messages WHERE group_id = -993076001 AND message_id = 1;
EXECUTE pin(-993076001, 1, -993076001, 103, 5000);
ROLLBACK;
`;
const result = spawnSync('psql', ['-XqAt', '-v', 'ON_ERROR_STOP=1', '-d', url], {
  input: sql, encoding: 'utf8',
});
assert.equal(result.status, 0, result.stderr || String(result.error));
assert.deepEqual(result.stdout.trim().split('\n').filter(Boolean), [
  '101|1000', '101|1000', '102|3000', '101|1000',
  '2|102|3000', '1|101|1000', '2', '103|5000',
]);
console.log('PASS: real PostgreSQL pin SQL: retry preserves pinner/time/order, multiple pins, idempotent delete, repin. Fixtures rolled back.');
