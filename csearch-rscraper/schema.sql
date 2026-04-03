-- Fresh database bootstrap for the csearch updater.
--
-- This file is intended to run once against an empty Postgres database, e.g.
-- via the official postgres image's /docker-entrypoint-initdb.d/ mechanism on
-- first container start.

BEGIN;

CREATE EXTENSION IF NOT EXISTS pg_trgm;

CREATE TABLE bills (
    billid              text,
    billnumber          integer NOT NULL,
    billtype            text NOT NULL,
    introducedat        date,
    congress            integer NOT NULL,
    summary_date        text,
    summary_text        text,
    sponsor_bioguide_id text,
    sponsor_name        text,
    sponsor_state       text,
    sponsor_party       text,
    origin_chamber      text,
    policy_area         text,
    update_date         date,
    latest_action_id    bigint,
    latest_action_date  date,
    bill_status         text NOT NULL,
    statusat            date NOT NULL,
    shorttitle          text,
    officialtitle       text,
    CONSTRAINT bill_pkey PRIMARY KEY (billtype, billnumber, congress)
) PARTITION BY LIST (congress);

-- One partition per congress, 93 through 119. A default partition absorbs any
-- future congress that does not yet have an explicit partition.
CREATE TABLE bills_93  PARTITION OF bills FOR VALUES IN (93);
CREATE TABLE bills_94  PARTITION OF bills FOR VALUES IN (94);
CREATE TABLE bills_95  PARTITION OF bills FOR VALUES IN (95);
CREATE TABLE bills_96  PARTITION OF bills FOR VALUES IN (96);
CREATE TABLE bills_97  PARTITION OF bills FOR VALUES IN (97);
CREATE TABLE bills_98  PARTITION OF bills FOR VALUES IN (98);
CREATE TABLE bills_99  PARTITION OF bills FOR VALUES IN (99);
CREATE TABLE bills_100 PARTITION OF bills FOR VALUES IN (100);
CREATE TABLE bills_101 PARTITION OF bills FOR VALUES IN (101);
CREATE TABLE bills_102 PARTITION OF bills FOR VALUES IN (102);
CREATE TABLE bills_103 PARTITION OF bills FOR VALUES IN (103);
CREATE TABLE bills_104 PARTITION OF bills FOR VALUES IN (104);
CREATE TABLE bills_105 PARTITION OF bills FOR VALUES IN (105);
CREATE TABLE bills_106 PARTITION OF bills FOR VALUES IN (106);
CREATE TABLE bills_107 PARTITION OF bills FOR VALUES IN (107);
CREATE TABLE bills_108 PARTITION OF bills FOR VALUES IN (108);
CREATE TABLE bills_109 PARTITION OF bills FOR VALUES IN (109);
CREATE TABLE bills_110 PARTITION OF bills FOR VALUES IN (110);
CREATE TABLE bills_111 PARTITION OF bills FOR VALUES IN (111);
CREATE TABLE bills_112 PARTITION OF bills FOR VALUES IN (112);
CREATE TABLE bills_113 PARTITION OF bills FOR VALUES IN (113);
CREATE TABLE bills_114 PARTITION OF bills FOR VALUES IN (114);
CREATE TABLE bills_115 PARTITION OF bills FOR VALUES IN (115);
CREATE TABLE bills_116 PARTITION OF bills FOR VALUES IN (116);
CREATE TABLE bills_117 PARTITION OF bills FOR VALUES IN (117);
CREATE TABLE bills_118 PARTITION OF bills FOR VALUES IN (118);
CREATE TABLE bills_119 PARTITION OF bills FOR VALUES IN (119);
CREATE TABLE bills_default PARTITION OF bills DEFAULT;

-- Weighted full-text search vector for bill search and ranking.
ALTER TABLE bills ADD COLUMN search_document tsvector GENERATED ALWAYS AS (
    setweight(to_tsvector('english', coalesce(shorttitle, '')), 'A') ||
    setweight(to_tsvector('english', coalesce(officialtitle, '')), 'A') ||
    setweight(to_tsvector('english', coalesce(summary_text, '')), 'B') ||
    setweight(to_tsvector('english', coalesce(sponsor_name, '')), 'C') ||
    setweight(to_tsvector('english', coalesce(policy_area, '')), 'C')
) STORED;

CREATE INDEX bills_search_document_idx ON bills USING GIN (search_document);

-- Billtype index to keep per-type queries fast without billtype partitioning.
CREATE INDEX bills_billtype_idx ON bills (billtype, congress);

CREATE INDEX bills_bill_status_idx ON bills (bill_status, congress);

-- Recency-oriented browse queries order by these two columns together.
CREATE INDEX bills_latest_action_date_idx
    ON bills (latest_action_date DESC NULLS LAST);

CREATE INDEX bills_statusat_update_date_idx
    ON bills (statusat DESC, update_date DESC NULLS LAST);

CREATE INDEX bills_missing_metadata_idx
    ON bills (congress, billtype, billnumber)
    WHERE shorttitle IS NULL
       OR officialtitle IS NULL
       OR sponsor_name IS NULL
       OR summary_text IS NULL
       OR policy_area IS NULL;

CREATE TABLE bill_actions (
    id                 bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    billtype           text NOT NULL,
    billnumber         integer NOT NULL,
    congress           integer NOT NULL,
    acted_at           date NOT NULL,
    action_text        text,
    action_type        text,
    action_code        text,
    source_system_code text,
    CONSTRAINT bill_actions_bill_fkey
        FOREIGN KEY (billtype, billnumber, congress)
        REFERENCES bills (billtype, billnumber, congress)
        ON DELETE CASCADE
);

CREATE UNIQUE INDEX bill_actions_id_bill_lookup_idx
    ON bill_actions (id, billtype, billnumber, congress);

CREATE INDEX bill_actions_bill_lookup_idx
    ON bill_actions (billtype, billnumber, congress, acted_at);

ALTER TABLE bills
    ADD CONSTRAINT bills_latest_action_fkey
        FOREIGN KEY (latest_action_id, billtype, billnumber, congress)
        REFERENCES bill_actions (id, billtype, billnumber, congress)
        ON DELETE SET NULL;

CREATE TABLE bill_cosponsors (
    billtype              text NOT NULL,
    billnumber            integer NOT NULL,
    congress              integer NOT NULL,
    bioguide_id           text NOT NULL,
    full_name             text,
    state                 text,
    party                 text,
    sponsorship_date      date,
    is_original_cosponsor boolean,
    CONSTRAINT bill_cosponsors_pkey PRIMARY KEY (billtype, billnumber, congress, bioguide_id),
    CONSTRAINT bill_cosponsors_bill_fkey
        FOREIGN KEY (billtype, billnumber, congress)
        REFERENCES bills (billtype, billnumber, congress)
        ON DELETE CASCADE
);

CREATE TABLE committees (
    committee_code text PRIMARY KEY,
    committee_name text,
    chamber        text
);

CREATE INDEX committees_chamber_idx
    ON committees (chamber);

CREATE TABLE bill_committees (
    billtype       text NOT NULL,
    billnumber     integer NOT NULL,
    congress       integer NOT NULL,
    committee_code text NOT NULL,
    CONSTRAINT bill_committees_pkey PRIMARY KEY (billtype, billnumber, congress, committee_code),
    CONSTRAINT bill_committees_bill_fkey
        FOREIGN KEY (billtype, billnumber, congress)
        REFERENCES bills (billtype, billnumber, congress)
        ON DELETE CASCADE,
    CONSTRAINT bill_committees_committee_fkey
        FOREIGN KEY (committee_code)
        REFERENCES committees (committee_code)
        ON DELETE CASCADE
);

CREATE INDEX bill_committees_code_idx
    ON bill_committees (committee_code, congress);

CREATE TABLE bill_subjects (
    billtype   text NOT NULL,
    billnumber integer NOT NULL,
    congress   integer NOT NULL,
    subject    text NOT NULL,
    CONSTRAINT bill_subjects_pkey PRIMARY KEY (billtype, billnumber, congress, subject),
    CONSTRAINT bill_subjects_bill_fkey
        FOREIGN KEY (billtype, billnumber, congress)
        REFERENCES bills (billtype, billnumber, congress)
        ON DELETE CASCADE
);

CREATE INDEX bill_subjects_subject_idx
    ON bill_subjects (subject, congress);

CREATE TABLE votes (
    voteid      text NOT NULL PRIMARY KEY,
    bill_type   text,
    bill_number integer,
    congress    integer,
    votenumber  integer,
    votedate    date,
    question    text,
    result      text,
    votesession text,
    chamber     text,
    source_url  text,
    votetype    text
);

ALTER TABLE votes ADD COLUMN search_document tsvector GENERATED ALWAYS AS (
    setweight(to_tsvector('english', coalesce(question, '')), 'A') ||
    setweight(to_tsvector('english', coalesce(result, '')), 'B') ||
    setweight(to_tsvector('english', coalesce(votetype, '')), 'C') ||
    setweight(to_tsvector('english', coalesce(chamber, '')), 'D')
) STORED;

CREATE INDEX votes_votedate_idx ON votes (votedate DESC);
CREATE INDEX votes_congress_idx ON votes (congress);
CREATE INDEX votes_chamber_idx ON votes (chamber);
CREATE INDEX votes_search_document_idx ON votes USING GIN (search_document);

CREATE TABLE vote_members (
    voteid       text NOT NULL,
    bioguide_id  text NOT NULL,
    display_name text,
    party        text,
    state        text,
    position     text NOT NULL,
    CONSTRAINT vote_members_pkey PRIMARY KEY (voteid, bioguide_id),
    CONSTRAINT vote_members_vote_fkey
        FOREIGN KEY (voteid)
        REFERENCES votes (voteid)
        ON DELETE CASCADE
);

CREATE INDEX vote_members_notvoting_idx
    ON vote_members (bioguide_id)
    WHERE position = 'notvoting';

CREATE OR REPLACE FUNCTION search_bills(
    search_query text,
    filter_billtype text DEFAULT NULL,
    filter_congress integer DEFAULT NULL,
    result_limit integer DEFAULT 50
) RETURNS TABLE (
    billtype text,
    billnumber text,
    congress text,
    shorttitle text,
    officialtitle text,
    summary_text text,
    sponsor_name text,
    policy_area text,
    rank real
) LANGUAGE sql STABLE AS $$
    WITH query AS (
        SELECT websearch_to_tsquery('english', search_query) AS ts_query
    )
    SELECT
        b.billtype,
        b.billnumber::text,
        b.congress::text,
        b.shorttitle,
        b.officialtitle,
        b.summary_text,
        b.sponsor_name,
        b.policy_area,
        ts_rank_cd(b.search_document, query.ts_query) AS rank
    FROM bills b
    CROSS JOIN query
    WHERE (filter_billtype IS NULL OR b.billtype = filter_billtype)
      AND (filter_congress IS NULL OR b.congress = filter_congress)
      AND b.search_document @@ query.ts_query
    ORDER BY rank DESC, b.statusat DESC, b.billtype, b.billnumber
    LIMIT GREATEST(result_limit, 1);
$$;

CREATE OR REPLACE FUNCTION search_votes(
    search_query text,
    filter_congress integer DEFAULT NULL,
    filter_chamber text DEFAULT NULL,
    result_limit integer DEFAULT 50
) RETURNS TABLE (
    voteid text,
    congress text,
    chamber text,
    votetype text,
    question text,
    result text,
    votedate text,
    rank real
) LANGUAGE sql STABLE AS $$
    WITH query AS (
        SELECT websearch_to_tsquery('english', search_query) AS ts_query
    )
    SELECT
        v.voteid,
        v.congress::text,
        v.chamber,
        v.votetype,
        v.question,
        v.result,
        v.votedate::text,
        ts_rank_cd(v.search_document, query.ts_query) AS rank
    FROM votes v
    CROSS JOIN query
    WHERE (filter_congress IS NULL OR v.congress = filter_congress)
      AND (filter_chamber IS NULL OR v.chamber = filter_chamber)
      AND v.search_document @@ query.ts_query
    ORDER BY rank DESC, v.votedate DESC, v.voteid DESC
    LIMIT GREATEST(result_limit, 1);
$$;

COMMIT;
