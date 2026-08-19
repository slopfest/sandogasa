-- Who submitted the builds, day by day.
--
-- This is how a mass rebuild is dated from evidence rather than from a
-- schedule: the days it runs are 79-95% `releng`, while a busy day of
-- continuous rebuilds is 90-98% `koschei` and behaves completely
-- differently. Fedora's published schedule allots weeks to a rebuild that
-- in practice burns through in about six days.
--
--   sqlite3 lag.sqlite < submitters-by-day.sql
--
-- Edit the window below.
WITH bounds AS (
    SELECT strftime('%s', '2026-07-01') AS lo,
           strftime('%s', '2026-08-01') AS hi
),
per_day AS (
    SELECT date(b.completion_ts, 'unixepoch') AS day,
           b.owner,
           count(*) AS builds
    FROM builds b, bounds
    WHERE b.instance = 'fedora'
      AND b.completion_ts >= bounds.lo AND b.completion_ts < bounds.hi
    GROUP BY 1, 2
),
totals AS (
    SELECT day, sum(builds) AS total FROM per_day GROUP BY 1
)
SELECT p.day,
       t.total AS builds,
       -- The submitter's account, minus the hostname koji appends to
       -- service accounts, which is noise at this granularity.
       substr(p.owner, 1, instr(p.owner || '/', '/') - 1) AS top_submitter,
       p.builds AS their_builds,
       round(100.0 * p.builds / t.total, 1) AS share_pct
FROM per_day p
JOIN totals t USING (day)
WHERE p.builds = (SELECT max(builds) FROM per_day q WHERE q.day = p.day)
ORDER BY p.day;
