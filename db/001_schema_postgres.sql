-- Consolidated PostgreSQL schema through 016_bounded_workloads (2026-09-05).
-- Fresh schemas only; rebuild the protected development database after consolidation.
-- Reference data and opt-in development fixtures: insert_postgres.sql.
-- Retention and cross-storage invariants: docs/retention-policy.md and AGENTS.md.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";
CREATE EXTENSION IF NOT EXISTS "uuid-ossp";
CREATE EXTENSION IF NOT EXISTS pg_trgm;

-- Tables, contraintes et index (parents avant leurs dépendants).
-- Les auto-références restent dans CREATE TABLE ; aucune clé étrangère n’est différée.

-- Comptes et authentification mobile

CREATE TABLE user_account (
    user_id uuid DEFAULT uuid_generate_v4() NOT NULL,
    role text DEFAULT 'user'::text NOT NULL,
    phone_number_hash text NOT NULL,
    phone_number_encrypted bytea NOT NULL,
    is_banned boolean DEFAULT false NOT NULL,
    banned_at timestamp with time zone,
    banned_reason text,
    banned_by uuid,
    deleted_at timestamp with time zone,
    anonymized_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT user_account_role_check CHECK ((role = ANY (ARRAY['user'::text, 'admin'::text, 'superadmin'::text]))),
    CONSTRAINT user_account_phone_number_hash_key UNIQUE (phone_number_hash),
    CONSTRAINT user_account_pkey PRIMARY KEY (user_id),
    CONSTRAINT user_account_banned_by_fkey FOREIGN KEY (banned_by) REFERENCES user_account(user_id) ON DELETE SET NULL
);

CREATE INDEX idx_user_account_active_created ON user_account USING btree (created_at DESC, user_id DESC) WHERE (deleted_at IS NULL);

CREATE TABLE account_tombstone (
    phone_number_hash text NOT NULL,
    reason text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    expires_at timestamp with time zone NOT NULL,
    CONSTRAINT account_tombstone_reason_check CHECK ((reason = 'banned_account'::text)),
    CONSTRAINT account_tombstone_pkey PRIMARY KEY (phone_number_hash)
);

CREATE INDEX idx_account_tombstone_expiry ON account_tombstone USING btree (expires_at, phone_number_hash);

CREATE TABLE otp_verification (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    phone_number_hash text NOT NULL,
    otp_hash text NOT NULL,
    expires_at timestamp with time zone NOT NULL,
    used boolean DEFAULT false NOT NULL,
    idempotency_key uuid DEFAULT uuid_generate_v4() NOT NULL,
    delivery_status text DEFAULT 'pending'::text NOT NULL,
    provider text DEFAULT 'sweego'::text NOT NULL,
    provider_transaction_id text,
    provider_message_id text,
    delivery_error_code text,
    sent_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    attempt_number bigint GENERATED ALWAYS AS IDENTITY NOT NULL,
    settlement_deadline timestamp with time zone DEFAULT (clock_timestamp() + '00:00:35'::interval) NOT NULL,
    provider_sent_at timestamp with time zone,
    failed_at timestamp with time zone,
    last_webhook_at timestamp with time zone,
    CONSTRAINT chk_otp_delivery_status CHECK ((delivery_status = ANY (ARRAY['pending'::text, 'accepted'::text, 'sent'::text, 'failed'::text, 'unknown'::text]))),
    CONSTRAINT otp_verification_pkey PRIMARY KEY (id)
);

COMMENT ON COLUMN otp_verification.sent_at IS 'First acceptance/enqueue confirmation observed locally; not proof of handset delivery.';

COMMENT ON COLUMN otp_verification.provider_sent_at IS 'Local receipt time of authenticated sms_sent; Sweego does not document this as handset delivery.';

CREATE INDEX idx_otp_expires ON otp_verification USING btree (expires_at, id);

CREATE UNIQUE INDEX idx_otp_idempotency ON otp_verification USING btree (idempotency_key);

CREATE UNIQUE INDEX idx_otp_one_usable_per_phone ON otp_verification USING btree (phone_number_hash) WHERE ((delivery_status = ANY (ARRAY['accepted'::text, 'sent'::text])) AND (used = false));

CREATE INDEX idx_otp_phone ON otp_verification USING btree (phone_number_hash);

CREATE INDEX idx_otp_phone_attempt ON otp_verification USING btree (phone_number_hash, attempt_number DESC);

CREATE TABLE refresh_token_family (
    id uuid NOT NULL,
    user_id uuid NOT NULL,
    created_at timestamp with time zone NOT NULL,
    last_refreshed_at timestamp with time zone NOT NULL,
    expires_at timestamp with time zone NOT NULL,
    revoked_at timestamp with time zone,
    revocation_reason text,
    CONSTRAINT chk_refresh_family_revocation CHECK ((((revoked_at IS NULL) AND (revocation_reason IS NULL)) OR ((revoked_at IS NOT NULL) AND (revocation_reason IS NOT NULL) AND (revocation_reason = ANY (ARRAY['replay'::text, 'logout'::text, 'logout_all'::text, 'user_revoked'::text, 'banned'::text, 'legacy_revoked'::text]))))),
    CONSTRAINT refresh_token_family_id_user_id_key UNIQUE (id, user_id),
    CONSTRAINT refresh_token_family_pkey PRIMARY KEY (id),
    CONSTRAINT refresh_token_family_user_id_fkey FOREIGN KEY (user_id) REFERENCES user_account(user_id) ON DELETE CASCADE
);

CREATE INDEX idx_refresh_family_expiry ON refresh_token_family USING btree (expires_at, id);

CREATE INDEX idx_refresh_family_user_created ON refresh_token_family USING btree (user_id, created_at DESC, id DESC);

CREATE TABLE refresh_tokens (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    user_id uuid NOT NULL,
    token_hash text NOT NULL,
    jti uuid NOT NULL,
    revoked boolean DEFAULT false NOT NULL,
    expires_at timestamp with time zone NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    family_id uuid NOT NULL,
    parent_token_id uuid,
    rotated_at timestamp with time zone,
    CONSTRAINT chk_refresh_not_own_parent CHECK ((parent_token_id IS DISTINCT FROM id)),
    CONSTRAINT chk_refresh_rotation CHECK (((rotated_at IS NULL) OR revoked)),
    CONSTRAINT refresh_tokens_jti_key UNIQUE (jti),
    CONSTRAINT refresh_tokens_pkey PRIMARY KEY (id),
    CONSTRAINT uq_refresh_id_family UNIQUE (id, family_id),
    CONSTRAINT fk_refresh_family_owner FOREIGN KEY (family_id, user_id) REFERENCES refresh_token_family(id, user_id) ON DELETE CASCADE,
    CONSTRAINT fk_refresh_parent FOREIGN KEY (parent_token_id, family_id) REFERENCES refresh_tokens(id, family_id) ON DELETE SET NULL (parent_token_id),
    CONSTRAINT refresh_tokens_user_id_fkey FOREIGN KEY (user_id) REFERENCES user_account(user_id) ON DELETE CASCADE
);

CREATE INDEX idx_refresh_tokens_expires ON refresh_tokens USING btree (expires_at, id);

CREATE INDEX idx_refresh_tokens_family ON refresh_tokens USING btree (family_id);

CREATE INDEX idx_refresh_tokens_user ON refresh_tokens USING btree (user_id);

CREATE UNIQUE INDEX uq_refresh_family_active ON refresh_tokens USING btree (family_id) WHERE (revoked = false);

CREATE UNIQUE INDEX uq_refresh_token_child ON refresh_tokens USING btree (parent_token_id) WHERE (parent_token_id IS NOT NULL);

CREATE TABLE device_token (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    user_id uuid NOT NULL,
    token text NOT NULL,
    platform text NOT NULL,
    app_version text,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    last_used_at timestamp with time zone,
    session_id uuid,
    CONSTRAINT device_token_app_version_check CHECK (((app_version IS NULL) OR ((octet_length(app_version) >= 1) AND (octet_length(app_version) <= 50)))),
    CONSTRAINT device_token_platform_check CHECK ((platform = ANY (ARRAY['ios'::text, 'android'::text]))),
    CONSTRAINT device_token_pkey PRIMARY KEY (id),
    CONSTRAINT device_token_token_key UNIQUE (token),
    CONSTRAINT device_token_user_id_fkey FOREIGN KEY (user_id) REFERENCES user_account(user_id) ON DELETE CASCADE,
    CONSTRAINT fk_device_session_owner FOREIGN KEY (session_id, user_id) REFERENCES refresh_token_family(id, user_id) ON DELETE CASCADE
);

CREATE INDEX idx_device_session ON device_token USING btree (session_id) WHERE (session_id IS NOT NULL);

CREATE INDEX idx_device_token_user ON device_token USING btree (user_id);

-- Profils, consentements et catalogues

CREATE TABLE user_profile (
    user_id uuid NOT NULL,
    firstname text NOT NULL,
    birthdate date NOT NULL,
    sex text,
    bio text,
    CONSTRAINT user_profile_sex_check CHECK ((sex = ANY (ARRAY['male'::text, 'female'::text, 'other'::text]))),
    CONSTRAINT user_profile_pkey PRIMARY KEY (user_id),
    CONSTRAINT user_profile_user_id_fkey FOREIGN KEY (user_id) REFERENCES user_account(user_id) ON DELETE CASCADE
);

CREATE INDEX idx_user_profile_discovery ON user_profile USING btree (sex, birthdate, user_id) WHERE (sex IS NOT NULL);

CREATE INDEX idx_user_profile_firstname_trgm ON user_profile USING gin (firstname gin_trgm_ops);

CREATE TABLE user_preferences (
    user_id uuid NOT NULL,
    min_age integer DEFAULT 18 NOT NULL,
    max_age integer DEFAULT 99 NOT NULL,
    max_distance_km integer DEFAULT 50 NOT NULL,
    looking_for text DEFAULT 'male'::text NOT NULL,
    CONSTRAINT chk_age_range CHECK (((min_age >= 18) AND (max_age >= min_age) AND (max_age <= 99))),
    CONSTRAINT chk_distance CHECK ((max_distance_km > 0)),
    CONSTRAINT user_preferences_looking_for_check CHECK ((looking_for = ANY (ARRAY['male'::text, 'female'::text, 'both'::text, 'other'::text]))),
    CONSTRAINT user_preferences_pkey PRIMARY KEY (user_id),
    CONSTRAINT user_preferences_user_id_fkey FOREIGN KEY (user_id) REFERENCES user_account(user_id) ON DELETE CASCADE
);

CREATE TABLE user_presence (
    user_id uuid NOT NULL,
    latitude numeric(9,6),
    longitude numeric(9,6),
    is_location_fresh boolean DEFAULT false NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT user_presence_pkey PRIMARY KEY (user_id),
    CONSTRAINT user_presence_user_id_fkey FOREIGN KEY (user_id) REFERENCES user_account(user_id) ON DELETE CASCADE
);

CREATE INDEX idx_user_presence_location ON user_presence USING btree (latitude, longitude) INCLUDE (user_id, updated_at) WHERE (is_location_fresh = true);

CREATE INDEX idx_user_presence_updated ON user_presence USING btree (updated_at, user_id);

CREATE TABLE user_consent (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    event_sequence bigserial NOT NULL,
    user_id uuid NOT NULL,
    consent_type text NOT NULL,
    granted boolean NOT NULL,
    document_version text NOT NULL,
    ip_address text,
    user_agent text,
    granted_at timestamp with time zone DEFAULT now() NOT NULL,
    withdrawn_at timestamp with time zone,
    CONSTRAINT user_consent_consent_type_check CHECK ((consent_type = ANY (ARRAY['terms_of_service_acceptance'::text, 'privacy_notice_acknowledgement'::text, 'sensitive_data_consent'::text, 'location_consent'::text]))),
    CONSTRAINT user_consent_pkey PRIMARY KEY (id),
    CONSTRAINT user_consent_user_id_fkey FOREIGN KEY (user_id) REFERENCES user_account(user_id) ON DELETE CASCADE
);

CREATE UNIQUE INDEX idx_consent_active ON user_consent USING btree (user_id, consent_type) WHERE ((withdrawn_at IS NULL) AND (granted = true));

CREATE UNIQUE INDEX idx_consent_event_sequence ON user_consent USING btree (event_sequence);

CREATE INDEX idx_consent_type ON user_consent USING btree (user_id, consent_type, event_sequence DESC);

CREATE INDEX idx_user_consent_user_event ON user_consent USING btree (user_id, event_sequence);

CREATE INDEX idx_consent_withdrawn ON user_consent USING btree (withdrawn_at, id) WHERE (withdrawn_at IS NOT NULL);

CREATE TABLE trait (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    name text NOT NULL,
    CONSTRAINT trait_name_key UNIQUE (name),
    CONSTRAINT trait_pkey PRIMARY KEY (id)
);

CREATE TABLE user_trait (
    user_id uuid NOT NULL,
    trait_id uuid NOT NULL,
    CONSTRAINT user_trait_pkey PRIMARY KEY (user_id, trait_id),
    CONSTRAINT user_trait_trait_id_fkey FOREIGN KEY (trait_id) REFERENCES trait(id) ON DELETE CASCADE,
    CONSTRAINT user_trait_user_id_fkey FOREIGN KEY (user_id) REFERENCES user_account(user_id) ON DELETE CASCADE
);

CREATE TABLE profile_question (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    code text NOT NULL,
    prompt text NOT NULL,
    category text NOT NULL,
    display_order integer DEFAULT 100 NOT NULL,
    created_at timestamp with time zone DEFAULT clock_timestamp() NOT NULL,
    updated_at timestamp with time zone DEFAULT clock_timestamp() NOT NULL,
    CONSTRAINT chk_profile_question_category CHECK ((category = ANY (ARRAY['daily_life'::text, 'personality'::text, 'interests'::text, 'relationships'::text, 'conversation'::text]))),
    CONSTRAINT chk_profile_question_code CHECK ((code ~ '^[a-z][a-z0-9_]{2,63}$'::text)),
    CONSTRAINT chk_profile_question_display_order CHECK (((display_order >= 0) AND (display_order <= 10000))),
    CONSTRAINT chk_profile_question_prompt CHECK (((prompt = btrim(prompt)) AND ((char_length(prompt) >= 3) AND (char_length(prompt) <= 200)) AND (octet_length(prompt) <= 500))),
    CONSTRAINT profile_question_code_key UNIQUE (code),
    CONSTRAINT profile_question_pkey PRIMARY KEY (id)
);

CREATE INDEX idx_profile_question_catalog ON profile_question USING btree (display_order, id);

CREATE UNIQUE INDEX uq_profile_question_prompt_ci ON profile_question USING btree (lower(prompt));

CREATE TABLE user_profile_answer (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    user_id uuid NOT NULL,
    question_id uuid NOT NULL,
    answer text NOT NULL,
    "position" smallint NOT NULL,
    created_at timestamp with time zone DEFAULT clock_timestamp() NOT NULL,
    updated_at timestamp with time zone DEFAULT clock_timestamp() NOT NULL,
    CONSTRAINT chk_user_profile_answer_position CHECK ((("position" >= 1) AND ("position" <= 3))),
    CONSTRAINT chk_user_profile_answer_text CHECK (((answer = btrim(answer)) AND ((char_length(answer) >= 10) AND (char_length(answer) <= 300)) AND (octet_length(answer) <= 1000))),
    CONSTRAINT uq_user_profile_answer_position UNIQUE (user_id, "position"),
    CONSTRAINT uq_user_profile_answer_question UNIQUE (user_id, question_id),
    CONSTRAINT user_profile_answer_pkey PRIMARY KEY (id),
    CONSTRAINT user_profile_answer_question_id_fkey FOREIGN KEY (question_id) REFERENCES profile_question(id) ON DELETE CASCADE,
    CONSTRAINT user_profile_answer_user_id_fkey FOREIGN KEY (user_id) REFERENCES user_profile(user_id) ON DELETE CASCADE
);

CREATE INDEX idx_user_profile_answer_question ON user_profile_answer USING btree (question_id);

-- Photos et modération

CREATE TABLE user_photo (
    id uuid NOT NULL,
    user_id uuid NOT NULL,
    object_key text NOT NULL,
    status text NOT NULL,
    mime_type text,
    size_bytes integer,
    width integer,
    height integer,
    sha256 bytea,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT chk_user_photo_height CHECK (((height IS NULL) OR ((height >= 1) AND (height <= 2048)))),
    CONSTRAINT chk_user_photo_metadata_completeness CHECK ((((mime_type IS NULL) AND (size_bytes IS NULL) AND (width IS NULL) AND (height IS NULL) AND (sha256 IS NULL)) OR ((mime_type IS NOT NULL) AND (size_bytes IS NOT NULL) AND (width IS NOT NULL) AND (height IS NOT NULL) AND (sha256 IS NOT NULL)))),
    CONSTRAINT chk_user_photo_mime_type CHECK (((mime_type IS NULL) OR (mime_type = 'image/webp'::text))),
    CONSTRAINT chk_user_photo_object_key CHECK ((object_key = (((('profile-photos/'::text || (user_id)::text) || '/'::text) || (id)::text) || '.webp'::text))),
    CONSTRAINT chk_user_photo_ready_metadata CHECK (((status <> 'ready'::text) OR ((mime_type IS NOT NULL) AND (size_bytes IS NOT NULL) AND (width IS NOT NULL) AND (height IS NOT NULL) AND (sha256 IS NOT NULL)))),
    CONSTRAINT chk_user_photo_sha256 CHECK (((sha256 IS NULL) OR (octet_length(sha256) = 32))),
    CONSTRAINT chk_user_photo_size CHECK (((size_bytes IS NULL) OR ((size_bytes >= 1) AND (size_bytes <= 500000)))),
    CONSTRAINT chk_user_photo_width CHECK (((width IS NULL) OR ((width >= 1) AND (width <= 2048)))),
    CONSTRAINT user_photo_status_check CHECK ((status = ANY (ARRAY['pending'::text, 'processing'::text, 'ready'::text, 'deleting'::text]))),
    CONSTRAINT user_photo_object_key_key UNIQUE (object_key),
    CONSTRAINT user_photo_pkey PRIMARY KEY (id),
    CONSTRAINT user_photo_user_id_fkey FOREIGN KEY (user_id) REFERENCES user_profile(user_id) ON DELETE CASCADE
);

COMMENT ON TABLE user_photo IS 'Private, versioned profile-photo objects and their recoverable cross-storage lifecycle.';

COMMENT ON COLUMN user_photo.object_key IS 'Provider-neutral private S3 key; never a public or signed URL.';

COMMENT ON COLUMN user_photo.sha256 IS 'SHA-256 digest of the normalized WebP bytes.';

CREATE INDEX idx_user_photo_cleanup ON user_photo USING btree (status, updated_at, id) WHERE (status = ANY (ARRAY['pending'::text, 'processing'::text, 'deleting'::text]));

CREATE INDEX idx_user_photo_reconciliation ON user_photo USING btree (updated_at DESC, id DESC) WHERE (status = ANY (ARRAY['pending'::text, 'processing'::text, 'deleting'::text]));

CREATE UNIQUE INDEX uq_user_photo_in_progress ON user_photo USING btree (user_id) WHERE (status = ANY (ARRAY['pending'::text, 'processing'::text]));

CREATE UNIQUE INDEX uq_user_photo_ready ON user_photo USING btree (user_id) WHERE (status = 'ready'::text);

CREATE TABLE photo_upload_request (
    user_id uuid NOT NULL,
    idempotency_key uuid NOT NULL,
    request_sha256 bytea NOT NULL,
    photo_id uuid,
    status text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    expires_at timestamp with time zone DEFAULT (now() + '24:00:00'::interval) NOT NULL,
    CONSTRAINT chk_photo_upload_request_expiry CHECK ((expires_at > created_at)),
    CONSTRAINT photo_upload_request_request_sha256_check CHECK ((octet_length(request_sha256) = 32)),
    CONSTRAINT photo_upload_request_status_check CHECK ((status = ANY (ARRAY['processing'::text, 'completed'::text, 'consumed'::text]))),
    CONSTRAINT photo_upload_request_photo_id_key UNIQUE (photo_id),
    CONSTRAINT photo_upload_request_pkey PRIMARY KEY (user_id, idempotency_key),
    CONSTRAINT photo_upload_request_photo_id_fkey FOREIGN KEY (photo_id) REFERENCES user_photo(id) ON DELETE SET NULL,
    CONSTRAINT photo_upload_request_user_id_fkey FOREIGN KEY (user_id) REFERENCES user_profile(user_id) ON DELETE CASCADE
);

COMMENT ON TABLE photo_upload_request IS 'Short-lived idempotency records for profile-photo uploads; request hashes never contain the source image.';

COMMENT ON COLUMN photo_upload_request.request_sha256 IS 'SHA-256 of the filename, declared MIME type and source bytes.';

CREATE INDEX idx_photo_upload_request_expires ON photo_upload_request USING btree (expires_at, user_id, idempotency_key);

CREATE TABLE content_moderation_case (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    user_id uuid NOT NULL,
    content_type text NOT NULL,
    photo_id uuid,
    bio_user_id uuid,
    profile_answer_id uuid,
    status text NOT NULL,
    reason_codes text[] DEFAULT ARRAY[]::text[] NOT NULL,
    policy_version text NOT NULL,
    version integer DEFAULT 1 NOT NULL,
    face_count smallint,
    sharpness_score double precision,
    nsfw_score double precision,
    face_detectable boolean,
    sharp_enough boolean,
    content_allowed boolean,
    reviewed_by uuid,
    reviewed_at timestamp with time zone,
    review_reason text,
    created_at timestamp with time zone DEFAULT clock_timestamp() NOT NULL,
    updated_at timestamp with time zone DEFAULT clock_timestamp() NOT NULL,
    CONSTRAINT chk_content_moderation_photo_checks CHECK (((content_type = 'photo'::text) OR ((face_count IS NULL) AND (sharpness_score IS NULL) AND (nsfw_score IS NULL) AND (face_detectable IS NULL) AND (sharp_enough IS NULL) AND (content_allowed IS NULL)))),
    CONSTRAINT chk_content_moderation_policy_version CHECK ((policy_version ~ '^[a-z0-9][a-z0-9_.-]{0,63}$'::text)),
    CONSTRAINT chk_content_moderation_reason_codes CHECK ((reason_codes <@ ARRAY['spam'::text, 'insult'::text, 'personal_contact'::text, 'sexual_content'::text, 'face_not_detected'::text, 'multiple_faces'::text, 'blurry'::text, 'explicit_image'::text, 'analysis_unavailable'::text, 'legacy_unreviewed'::text])),
    CONSTRAINT chk_content_moderation_review_reason CHECK (((review_reason IS NULL) OR ((review_reason = btrim(review_reason)) AND ((char_length(review_reason) >= 3) AND (char_length(review_reason) <= 500)) AND (octet_length(review_reason) <= 1000)))),
    CONSTRAINT chk_content_moderation_source CHECK ((((content_type = 'photo'::text) AND (photo_id IS NOT NULL) AND (bio_user_id IS NULL) AND (profile_answer_id IS NULL)) OR ((content_type = 'bio'::text) AND (photo_id IS NULL) AND (bio_user_id IS NOT NULL) AND (profile_answer_id IS NULL)) OR ((content_type = 'profile_answer'::text) AND (photo_id IS NULL) AND (bio_user_id IS NULL) AND (profile_answer_id IS NOT NULL)))),
    CONSTRAINT content_moderation_case_content_type_check CHECK ((content_type = ANY (ARRAY['photo'::text, 'bio'::text, 'profile_answer'::text]))),
    CONSTRAINT content_moderation_case_face_count_check CHECK (((face_count >= 0) AND (face_count <= 100))),
    CONSTRAINT content_moderation_case_nsfw_score_check CHECK (((nsfw_score >= (0)::double precision) AND (nsfw_score <= (1)::double precision))),
    CONSTRAINT content_moderation_case_sharpness_score_check CHECK (((sharpness_score >= (0)::double precision) AND (sharpness_score < ('1000000000000'::bigint)::double precision))),
    CONSTRAINT content_moderation_case_status_check CHECK ((status = ANY (ARRAY['pending'::text, 'approved'::text, 'rejected'::text]))),
    CONSTRAINT content_moderation_case_version_check CHECK ((version > 0)),
    CONSTRAINT content_moderation_case_pkey PRIMARY KEY (id),
    CONSTRAINT content_moderation_case_bio_user_id_fkey FOREIGN KEY (bio_user_id) REFERENCES user_profile(user_id) ON DELETE CASCADE,
    CONSTRAINT content_moderation_case_photo_id_fkey FOREIGN KEY (photo_id) REFERENCES user_photo(id) ON DELETE CASCADE,
    CONSTRAINT content_moderation_case_profile_answer_id_fkey FOREIGN KEY (profile_answer_id) REFERENCES user_profile_answer(id) ON DELETE CASCADE,
    CONSTRAINT content_moderation_case_reviewed_by_fkey FOREIGN KEY (reviewed_by) REFERENCES user_account(user_id) ON DELETE SET NULL,
    CONSTRAINT content_moderation_case_user_id_fkey FOREIGN KEY (user_id) REFERENCES user_account(user_id) ON DELETE CASCADE
);

COMMENT ON TABLE content_moderation_case IS 'Current moderation decision for one active photo, bio, or profile answer. Source content remains in its domain table.';

COMMENT ON COLUMN content_moderation_case.reason_codes IS 'Closed, non-sensitive signal codes only; never provider output or user content.';

CREATE INDEX idx_content_moderation_queue ON content_moderation_case USING btree (status, updated_at DESC, id DESC);

CREATE INDEX idx_content_moderation_type_updated ON content_moderation_case USING btree (content_type, updated_at DESC, id DESC);

CREATE INDEX idx_content_moderation_updated ON content_moderation_case USING btree (updated_at DESC, id DESC);

CREATE INDEX idx_content_moderation_user ON content_moderation_case USING btree (user_id, updated_at DESC);

CREATE UNIQUE INDEX uq_content_moderation_bio ON content_moderation_case USING btree (bio_user_id) WHERE (bio_user_id IS NOT NULL);

CREATE UNIQUE INDEX uq_content_moderation_photo ON content_moderation_case USING btree (photo_id) WHERE (photo_id IS NOT NULL);

CREATE UNIQUE INDEX uq_content_moderation_profile_answer ON content_moderation_case USING btree (profile_answer_id) WHERE (profile_answer_id IS NOT NULL);

-- Abonnements et facturation

CREATE TABLE subscription_plan (
    code text NOT NULL,
    display_name text NOT NULL,
    monthly_price_cents integer NOT NULL,
    annual_price_cents integer NOT NULL,
    currency character(3) DEFAULT 'EUR'::bpchar NOT NULL,
    weekly_continuation_limit smallint,
    trial_days smallint DEFAULT 0 NOT NULL,
    is_active boolean DEFAULT true NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT subscription_plan_annual_price_cents_check CHECK ((annual_price_cents >= 0)),
    CONSTRAINT subscription_plan_check CHECK ((annual_price_cents <= (monthly_price_cents * 12))),
    CONSTRAINT subscription_plan_currency_check CHECK ((currency ~ '^[A-Z]{3}$'::text)),
    CONSTRAINT subscription_plan_monthly_price_cents_check CHECK ((monthly_price_cents >= 0)),
    CONSTRAINT subscription_plan_trial_days_check CHECK ((trial_days >= 0)),
    CONSTRAINT subscription_plan_weekly_continuation_limit_check CHECK (((weekly_continuation_limit IS NULL) OR (weekly_continuation_limit >= 0))),
    CONSTRAINT subscription_plan_pkey PRIMARY KEY (code)
);

CREATE TABLE subscription_plan_feature (
    plan_code text NOT NULL,
    feature_code text NOT NULL,
    display_name text NOT NULL,
    description text NOT NULL,
    feature_value jsonb DEFAULT '{}'::jsonb NOT NULL,
    sort_order smallint DEFAULT 0 NOT NULL,
    CONSTRAINT subscription_plan_feature_pkey PRIMARY KEY (plan_code, feature_code),
    CONSTRAINT subscription_plan_feature_plan_code_fkey FOREIGN KEY (plan_code) REFERENCES subscription_plan(code) ON DELETE CASCADE
);

CREATE TABLE user_subscription (
    user_id uuid NOT NULL,
    plan text DEFAULT 'free'::text NOT NULL,
    provider text,
    provider_subscription_id text,
    provider_price_id text,
    billing_period text,
    status text,
    cancel_at_period_end boolean DEFAULT false NOT NULL,
    current_period_starts_at timestamp with time zone,
    current_period_ends_at timestamp with time zone,
    trial_ends_at timestamp with time zone,
    canceled_at timestamp with time zone,
    provider_event_created_at timestamp with time zone,
    projection_version bigint DEFAULT 0 NOT NULL,
    provider_snapshot_at timestamp with time zone,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT user_subscription_billing_period_check CHECK (((billing_period IS NULL) OR (billing_period = ANY (ARRAY['monthly'::text, 'annual'::text])))),
    CONSTRAINT user_subscription_provider_check CHECK (((provider IS NULL) OR (provider = 'stripe'::text))),
    CONSTRAINT user_subscription_provider_price_id_check CHECK (((provider_price_id IS NULL) OR (provider_price_id ~ '^price_[A-Za-z0-9]+$'::text))),
    CONSTRAINT user_subscription_provider_subscription_id_check CHECK (((provider_subscription_id IS NULL) OR (provider_subscription_id ~ '^sub_[A-Za-z0-9]+$'::text))),
    CONSTRAINT user_subscription_projection_version_check CHECK ((projection_version >= 0)),
    CONSTRAINT user_subscription_status_check CHECK (((status IS NULL) OR (status = ANY (ARRAY['incomplete'::text, 'incomplete_expired'::text, 'trialing'::text, 'active'::text, 'past_due'::text, 'canceled'::text, 'unpaid'::text, 'paused'::text])))),
    CONSTRAINT user_subscription_pkey PRIMARY KEY (user_id),
    CONSTRAINT user_subscription_provider_subscription_id_key UNIQUE (provider_subscription_id),
    CONSTRAINT user_subscription_plan_fkey FOREIGN KEY (plan) REFERENCES subscription_plan(code),
    CONSTRAINT user_subscription_user_id_fkey FOREIGN KEY (user_id) REFERENCES user_account(user_id) ON DELETE CASCADE
);

CREATE INDEX idx_user_subscription_plan_updated ON user_subscription USING btree (plan, updated_at);

CREATE TABLE billing_customer (
    user_id uuid NOT NULL,
    stripe_customer_id text NOT NULL,
    stripe_customer_deleted_at timestamp with time zone,
    trial_used_at timestamp with time zone,
    stripe_reconciliation_due_at timestamp with time zone DEFAULT clock_timestamp() NOT NULL,
    stripe_reconciled_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT billing_customer_reconciliation_order_check CHECK (((stripe_reconciled_at IS NULL) OR (stripe_reconciliation_due_at >= stripe_reconciled_at))),
    CONSTRAINT billing_customer_stripe_customer_id_check CHECK ((stripe_customer_id ~ '^cus_[A-Za-z0-9]+$'::text)),
    CONSTRAINT billing_customer_pkey PRIMARY KEY (user_id),
    CONSTRAINT billing_customer_stripe_customer_id_key UNIQUE (stripe_customer_id),
    CONSTRAINT billing_customer_user_id_fkey FOREIGN KEY (user_id) REFERENCES user_account(user_id) ON DELETE CASCADE
);

CREATE INDEX idx_billing_customer_active_stripe_id ON billing_customer USING btree (stripe_customer_id) WHERE (stripe_customer_deleted_at IS NULL);

CREATE INDEX idx_billing_customer_reconciliation_due ON billing_customer USING btree (stripe_reconciliation_due_at, user_id) WHERE (stripe_customer_deleted_at IS NULL);

CREATE TABLE billing_checkout_session (
    id uuid NOT NULL,
    user_id uuid NOT NULL,
    idempotency_key uuid NOT NULL,
    billing_period text NOT NULL,
    stripe_session_id text,
    checkout_url text,
    status text NOT NULL,
    expires_at timestamp with time zone NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    customer_creation_started_at timestamp with time zone,
    created_customer_id text,
    customer_erased_at timestamp with time zone,
    CONSTRAINT billing_checkout_session_billing_period_check CHECK ((billing_period = ANY (ARRAY['monthly'::text, 'annual'::text]))),
    CONSTRAINT billing_checkout_session_checkout_url_check CHECK (((checkout_url IS NULL) OR (octet_length(checkout_url) <= 4096))),
    CONSTRAINT billing_checkout_created_customer_id_check CHECK (((created_customer_id IS NULL) OR (created_customer_id ~ '^cus_[A-Za-z0-9]+$'::text))),
    CONSTRAINT billing_checkout_session_status_check CHECK ((status = ANY (ARRAY['creating'::text, 'open'::text, 'completed'::text, 'expired'::text, 'failed'::text]))),
    CONSTRAINT billing_checkout_session_stripe_session_id_check CHECK (((stripe_session_id IS NULL) OR (stripe_session_id ~ '^cs_(test_|live_)?[A-Za-z0-9]+$'::text))),
    CONSTRAINT billing_checkout_session_pkey PRIMARY KEY (id),
    CONSTRAINT billing_checkout_session_stripe_session_id_key UNIQUE (stripe_session_id),
    CONSTRAINT billing_checkout_session_user_id_idempotency_key_key UNIQUE (user_id, idempotency_key),
    CONSTRAINT billing_checkout_session_user_id_fkey FOREIGN KEY (user_id) REFERENCES user_account(user_id) ON DELETE CASCADE
);

CREATE INDEX idx_billing_checkout_expiry ON billing_checkout_session USING btree (expires_at) WHERE (status = ANY (ARRAY['creating'::text, 'open'::text]));

CREATE UNIQUE INDEX idx_billing_checkout_one_live_per_user ON billing_checkout_session USING btree (user_id) WHERE (status = ANY (ARRAY['creating'::text, 'open'::text]));

CREATE INDEX idx_checkout_customer_erasure ON billing_checkout_session USING btree (user_id, id) WHERE ((customer_creation_started_at IS NOT NULL) AND (customer_erased_at IS NULL));

CREATE TABLE billing_invoice (
    stripe_invoice_id text NOT NULL,
    user_id uuid,
    stripe_customer_id text NOT NULL,
    stripe_subscription_id text,
    status text,
    currency character(3) NOT NULL,
    amount_due bigint NOT NULL,
    amount_paid bigint NOT NULL,
    amount_remaining bigint NOT NULL,
    period_starts_at timestamp with time zone NOT NULL,
    period_ends_at timestamp with time zone NOT NULL,
    paid_at timestamp with time zone,
    created_at timestamp with time zone NOT NULL,
    provider_event_created_at timestamp with time zone,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT billing_invoice_amount_due_check CHECK ((amount_due >= 0)),
    CONSTRAINT billing_invoice_amount_paid_check CHECK ((amount_paid >= 0)),
    CONSTRAINT billing_invoice_amount_remaining_check CHECK ((amount_remaining >= 0)),
    CONSTRAINT billing_invoice_currency_check CHECK ((currency ~ '^[A-Z]{3}$'::text)),
    CONSTRAINT billing_invoice_stripe_customer_id_check CHECK ((stripe_customer_id ~ '^cus_[A-Za-z0-9]+$'::text)),
    CONSTRAINT billing_invoice_stripe_invoice_id_check CHECK ((stripe_invoice_id ~ '^in_[A-Za-z0-9]+$'::text)),
    CONSTRAINT billing_invoice_stripe_subscription_id_check CHECK (((stripe_subscription_id IS NULL) OR (stripe_subscription_id ~ '^sub_[A-Za-z0-9]+$'::text))),
    CONSTRAINT billing_invoice_pkey PRIMARY KEY (stripe_invoice_id),
    CONSTRAINT billing_invoice_user_id_fkey FOREIGN KEY (user_id) REFERENCES user_account(user_id) ON DELETE SET NULL
);

CREATE INDEX idx_billing_invoice_user_created ON billing_invoice USING btree (user_id, created_at DESC) WHERE (user_id IS NOT NULL);

CREATE INDEX idx_billing_invoice_user_created_id ON billing_invoice USING btree (user_id, created_at, stripe_invoice_id) WHERE (user_id IS NOT NULL);

CREATE TABLE stripe_webhook_event (
    id text NOT NULL,
    event_type text NOT NULL,
    object_id text,
    livemode boolean NOT NULL,
    api_version text,
    stripe_created_at timestamp with time zone NOT NULL,
    processed_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT stripe_webhook_event_event_type_check CHECK (((octet_length(event_type) >= 1) AND (octet_length(event_type) <= 150))),
    CONSTRAINT stripe_webhook_event_id_check CHECK ((id ~ '^evt_[A-Za-z0-9]+$'::text)),
    CONSTRAINT stripe_webhook_event_pkey PRIMARY KEY (id)
);

CREATE INDEX idx_stripe_webhook_processed ON stripe_webhook_event USING btree (processed_at DESC);

-- Matchs, messages et sûreté

CREATE TABLE match_init (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    user1_id uuid NOT NULL,
    user2_id uuid NOT NULL,
    status text DEFAULT 'active'::text NOT NULL,
    expires_at timestamp with time zone DEFAULT (now() + '24:00:00'::interval) NOT NULL,
    purge_after timestamp with time zone,
    continuation_initiator_id uuid,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    last_message_at timestamp with time zone,
    CONSTRAINT match_init_check CHECK ((user1_id < user2_id)),
    CONSTRAINT match_init_status_check CHECK ((status = ANY (ARRAY['awaiting_continuation'::text, 'active'::text, 'confirmed'::text, 'expired'::text, 'ended'::text]))),
    CONSTRAINT match_init_pkey PRIMARY KEY (id),
    CONSTRAINT match_init_user1_id_user2_id_key UNIQUE (user1_id, user2_id),
    CONSTRAINT match_init_continuation_initiator_id_fkey FOREIGN KEY (continuation_initiator_id) REFERENCES user_account(user_id) ON DELETE SET NULL,
    CONSTRAINT match_init_user1_id_fkey FOREIGN KEY (user1_id) REFERENCES user_account(user_id) ON DELETE CASCADE,
    CONSTRAINT match_init_user2_id_fkey FOREIGN KEY (user2_id) REFERENCES user_account(user_id) ON DELETE CASCADE
);

CREATE INDEX idx_match_init_activity ON match_init USING btree (COALESCE(last_message_at, created_at) DESC, id DESC);

CREATE INDEX idx_match_init_to_expire ON match_init USING btree (expires_at) WHERE (status = ANY (ARRAY['active'::text, 'awaiting_continuation'::text]));

CREATE INDEX idx_match_init_to_purge ON match_init USING btree (purge_after) WHERE (status = ANY (ARRAY['expired'::text, 'ended'::text]));

CREATE INDEX idx_match_init_user1 ON match_init USING btree (user1_id);

CREATE INDEX idx_match_init_user1_created ON match_init USING btree (user1_id, created_at, id);

CREATE INDEX idx_match_init_user1_activity ON match_init USING btree (user1_id, COALESCE(last_message_at, created_at) DESC, id DESC) WHERE (status <> 'ended'::text);

CREATE INDEX idx_match_init_user2 ON match_init USING btree (user2_id);

CREATE INDEX idx_match_init_user2_created ON match_init USING btree (user2_id, created_at, id);

CREATE INDEX idx_match_init_user2_activity ON match_init USING btree (user2_id, COALESCE(last_message_at, created_at) DESC, id DESC) WHERE (status <> 'ended'::text);

CREATE TABLE match_state (
    match_id uuid NOT NULL,
    user_id uuid NOT NULL,
    revealed boolean DEFAULT false NOT NULL,
    continued boolean DEFAULT false NOT NULL,
    CONSTRAINT match_state_pkey PRIMARY KEY (match_id, user_id),
    CONSTRAINT match_state_match_id_fkey FOREIGN KEY (match_id) REFERENCES match_init(id) ON DELETE CASCADE,
    CONSTRAINT match_state_user_id_fkey FOREIGN KEY (user_id) REFERENCES user_account(user_id) ON DELETE CASCADE
);

CREATE INDEX idx_match_state_user ON match_state USING btree (user_id);

CREATE TABLE chat_message (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    match_id uuid NOT NULL,
    sender_id uuid NOT NULL,
    idempotency_key uuid,
    content text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    read_at timestamp with time zone,
    CONSTRAINT chat_message_pkey PRIMARY KEY (id),
    CONSTRAINT chat_message_match_id_fkey FOREIGN KEY (match_id) REFERENCES match_init(id) ON DELETE CASCADE,
    CONSTRAINT chat_message_sender_id_fkey FOREIGN KEY (sender_id) REFERENCES user_account(user_id) ON DELETE CASCADE
);

CREATE INDEX idx_chat_message_match_unread ON chat_message USING btree (match_id, created_at DESC, id DESC) INCLUDE (sender_id) WHERE (read_at IS NULL);

CREATE INDEX idx_chat_message_sender_created ON chat_message USING btree (sender_id, created_at, id);

CREATE UNIQUE INDEX idx_chat_message_sender_idempotency ON chat_message USING btree (sender_id, idempotency_key) WHERE (idempotency_key IS NOT NULL);

CREATE INDEX idx_message_match_created_desc ON chat_message USING btree (match_id, created_at DESC, id DESC);

CREATE TABLE continuation_usage (
    user_id uuid NOT NULL,
    week_start date NOT NULL,
    used_count smallint DEFAULT 0 NOT NULL,
    CONSTRAINT continuation_usage_used_count_check CHECK ((used_count >= 0)),
    CONSTRAINT continuation_usage_pkey PRIMARY KEY (user_id, week_start),
    CONSTRAINT continuation_usage_user_id_fkey FOREIGN KEY (user_id) REFERENCES user_account(user_id) ON DELETE CASCADE
);

CREATE TABLE user_block (
    blocker_id uuid NOT NULL,
    blocked_id uuid NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT user_block_check CHECK ((blocker_id <> blocked_id)),
    CONSTRAINT user_block_pkey PRIMARY KEY (blocker_id, blocked_id),
    CONSTRAINT user_block_blocked_id_fkey FOREIGN KEY (blocked_id) REFERENCES user_account(user_id) ON DELETE CASCADE,
    CONSTRAINT user_block_blocker_id_fkey FOREIGN KEY (blocker_id) REFERENCES user_account(user_id) ON DELETE CASCADE
);

CREATE INDEX idx_user_block_blocked ON user_block USING btree (blocked_id);

CREATE INDEX idx_user_block_blocker_created ON user_block USING btree (blocker_id, created_at DESC, blocked_id);

CREATE TABLE user_report (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    reporter_id uuid NOT NULL,
    reported_id uuid NOT NULL,
    match_id uuid,
    reason text NOT NULL,
    description text,
    status text DEFAULT 'pending'::text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    resolved_at timestamp with time zone,
    CONSTRAINT user_report_check CHECK ((reporter_id <> reported_id)),
    CONSTRAINT user_report_reason_check CHECK ((reason = ANY (ARRAY['inappropriate_content'::text, 'fake_profile'::text, 'harassment'::text, 'spam'::text, 'other'::text]))),
    CONSTRAINT user_report_status_check CHECK ((status = ANY (ARRAY['pending'::text, 'reviewed'::text, 'dismissed'::text]))),
    CONSTRAINT user_report_pkey PRIMARY KEY (id),
    CONSTRAINT user_report_match_id_fkey FOREIGN KEY (match_id) REFERENCES match_init(id) ON DELETE SET NULL,
    CONSTRAINT user_report_reported_id_fkey FOREIGN KEY (reported_id) REFERENCES user_account(user_id) ON DELETE CASCADE,
    CONSTRAINT user_report_reporter_id_fkey FOREIGN KEY (reporter_id) REFERENCES user_account(user_id) ON DELETE CASCADE
);

CREATE INDEX idx_user_report_created_desc ON user_report USING btree (created_at DESC, id DESC);

CREATE INDEX idx_user_report_match_created ON user_report USING btree (match_id, created_at, id) WHERE (match_id IS NOT NULL);

CREATE UNIQUE INDEX idx_user_report_one_pending ON user_report USING btree (reporter_id, reported_id) WHERE (status = 'pending'::text);

CREATE INDEX idx_user_report_reported ON user_report USING btree (reported_id);

CREATE INDEX idx_user_report_reporter_created ON user_report USING btree (reporter_id, created_at, id);

CREATE INDEX idx_user_report_resolved ON user_report USING btree (resolved_at, id) WHERE (status = ANY (ARRAY['reviewed'::text, 'dismissed'::text]));

CREATE INDEX idx_user_report_status_created_desc ON user_report USING btree (status, created_at DESC, id DESC);

-- Notifications, outbox et maintenance

CREATE TABLE notification (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    user_id uuid NOT NULL,
    type text NOT NULL,
    payload jsonb,
    read_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    expires_at timestamp with time zone DEFAULT (now() + '90 days'::interval) NOT NULL,
    deduplication_key text,
    billing_reference text,
    billing_trial_ends_at timestamp with time zone,
    CONSTRAINT chk_notification_billing_context CHECK ((((billing_reference IS NULL) AND (billing_trial_ends_at IS NULL)) OR ((type = 'billing_payment_failed'::text) AND (billing_reference IS NOT NULL) AND (billing_reference ~ '^in_[A-Za-z0-9]+$'::text) AND (billing_trial_ends_at IS NULL)) OR ((type = 'subscription_trial_ending'::text) AND (billing_reference IS NOT NULL) AND (billing_reference ~ '^sub_[A-Za-z0-9]+$'::text) AND (billing_trial_ends_at IS NOT NULL)))),
    CONSTRAINT notification_deduplication_key_check CHECK (((deduplication_key IS NULL) OR (deduplication_key ~ '^[0-9a-f]{64}$'::text))),
    CONSTRAINT notification_type_check CHECK ((type = ANY (ARRAY['new_match'::text, 'new_message'::text, 'profile_liked'::text, 'match_expiring'::text, 'billing_payment_failed'::text, 'subscription_trial_ending'::text]))),
    CONSTRAINT notification_deduplication_key_key UNIQUE (deduplication_key),
    CONSTRAINT notification_pkey PRIMARY KEY (id),
    CONSTRAINT notification_user_id_fkey FOREIGN KEY (user_id) REFERENCES user_account(user_id) ON DELETE CASCADE
);

CREATE INDEX idx_notification_expire ON notification USING btree (expires_at, id);

CREATE INDEX idx_notification_unread ON notification USING btree (user_id, read_at) WHERE (read_at IS NULL);

CREATE INDEX idx_notification_user ON notification USING btree (user_id, created_at DESC);

CREATE TABLE outbox_event (
    id uuid NOT NULL,
    event_type text NOT NULL,
    aggregate_id uuid NOT NULL,
    payload jsonb DEFAULT '{}'::jsonb NOT NULL,
    status text DEFAULT 'pending'::text NOT NULL,
    attempts smallint DEFAULT 0 NOT NULL,
    available_at timestamp with time zone DEFAULT now() NOT NULL,
    locked_at timestamp with time zone,
    locked_by uuid,
    last_error_code text,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    processed_at timestamp with time zone,
    dead_lettered_at timestamp with time zone,
    resolved_at timestamp with time zone,
    resolved_by uuid,
    resolution_reason text,
    CONSTRAINT chk_outbox_event_completion CHECK ((((status = 'completed'::text) AND (processed_at IS NOT NULL)) OR ((status <> 'completed'::text) AND (processed_at IS NULL)))),
    CONSTRAINT chk_outbox_event_dead_letter CHECK ((((status = ANY (ARRAY['dead_letter'::text, 'discarded'::text])) AND (dead_lettered_at IS NOT NULL)) OR ((status <> ALL (ARRAY['dead_letter'::text, 'discarded'::text])) AND (dead_lettered_at IS NULL)))),
    CONSTRAINT chk_outbox_event_lock CHECK ((((status = 'processing'::text) AND (locked_at IS NOT NULL) AND (locked_by IS NOT NULL)) OR ((status <> 'processing'::text) AND (locked_at IS NULL) AND (locked_by IS NULL)))),
    CONSTRAINT chk_outbox_event_resolution CHECK ((((status = 'discarded'::text) AND (resolved_at IS NOT NULL) AND (resolution_reason IS NOT NULL) AND ((char_length(resolution_reason) >= 3) AND (char_length(resolution_reason) <= 500))) OR ((status <> 'discarded'::text) AND (resolved_at IS NULL) AND (resolved_by IS NULL) AND (resolution_reason IS NULL)))),
    CONSTRAINT outbox_event_attempts_check CHECK (((attempts >= 0) AND (attempts <= 100))),
    CONSTRAINT outbox_event_event_type_check CHECK ((event_type ~ '^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)+$'::text)),
    CONSTRAINT outbox_event_last_error_code_check CHECK (((last_error_code IS NULL) OR (last_error_code ~ '^[a-z][a-z0-9_]{0,63}$'::text))),
    CONSTRAINT outbox_event_payload_check CHECK ((jsonb_typeof(payload) = 'object'::text)),
    CONSTRAINT outbox_event_status_check CHECK ((status = ANY (ARRAY['pending'::text, 'processing'::text, 'completed'::text, 'dead_letter'::text, 'discarded'::text]))),
    CONSTRAINT outbox_event_event_type_aggregate_id_key UNIQUE (event_type, aggregate_id),
    CONSTRAINT outbox_event_pkey PRIMARY KEY (id),
    CONSTRAINT outbox_event_resolved_by_fkey FOREIGN KEY (resolved_by) REFERENCES user_account(user_id) ON DELETE SET NULL
);

COMMENT ON TABLE outbox_event IS 'Durable transactional events claimed by bounded workers with retry and dead-letter handling.';

COMMENT ON COLUMN outbox_event.payload IS 'Non-sensitive versioned event data; handlers should prefer aggregate identifiers over copied personal data.';

CREATE INDEX idx_outbox_event_completed ON outbox_event USING btree (processed_at, id) WHERE (status = 'completed'::text);

CREATE INDEX idx_outbox_event_dead_letter ON outbox_event USING btree (dead_lettered_at, id) WHERE (status = 'dead_letter'::text);

CREATE INDEX idx_outbox_event_discarded ON outbox_event USING btree (resolved_at, id) WHERE (status = 'discarded'::text);

CREATE INDEX idx_outbox_event_due ON outbox_event USING btree (available_at, created_at, id) WHERE (status = 'pending'::text);

CREATE INDEX idx_outbox_event_stale_lock ON outbox_event USING btree (locked_at, id) WHERE (status = 'processing'::text);

CREATE TABLE notification_push_delivery (
    id uuid NOT NULL,
    notification_id uuid NOT NULL,
    device_id uuid NOT NULL,
    session_id uuid,
    CONSTRAINT notification_push_delivery_notification_id_device_id_key UNIQUE (notification_id, device_id),
    CONSTRAINT notification_push_delivery_pkey PRIMARY KEY (id),
    CONSTRAINT notification_push_delivery_device_id_fkey FOREIGN KEY (device_id) REFERENCES device_token(id) ON DELETE CASCADE,
    CONSTRAINT notification_push_delivery_id_fkey FOREIGN KEY (id) REFERENCES outbox_event(id) ON DELETE CASCADE,
    CONSTRAINT notification_push_delivery_notification_id_fkey FOREIGN KEY (notification_id) REFERENCES notification(id) ON DELETE CASCADE
);

COMMENT ON TABLE notification_push_delivery IS 'Pending or completed per-device delivery references; expires with the notification, device, or resolved outbox event. No provider token is copied.';

CREATE INDEX idx_notification_push_device ON notification_push_delivery USING btree (device_id);

CREATE TABLE outbox_operator_action (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    outbox_event_id uuid,
    administrator_id uuid,
    administrator_role text NOT NULL,
    event_type text NOT NULL,
    action text NOT NULL,
    reason text NOT NULL,
    created_at timestamp with time zone DEFAULT clock_timestamp() NOT NULL,
    CONSTRAINT outbox_operator_action_action_check CHECK ((action = ANY (ARRAY['retry'::text, 'discard'::text]))),
    CONSTRAINT outbox_operator_action_administrator_role_check CHECK ((administrator_role = ANY (ARRAY['admin'::text, 'superadmin'::text]))),
    CONSTRAINT outbox_operator_action_reason_check CHECK (((char_length(reason) >= 3) AND (char_length(reason) <= 500))),
    CONSTRAINT outbox_operator_action_pkey PRIMARY KEY (id),
    CONSTRAINT outbox_operator_action_administrator_id_fkey FOREIGN KEY (administrator_id) REFERENCES user_account(user_id) ON DELETE SET NULL,
    CONSTRAINT outbox_operator_action_outbox_event_id_fkey FOREIGN KEY (outbox_event_id) REFERENCES outbox_event(id) ON DELETE SET NULL
);

COMMENT ON TABLE outbox_operator_action IS 'Audited administrator decisions for dead-letter retries and safe discards.';

CREATE INDEX idx_outbox_operator_action_admin ON outbox_operator_action USING btree (administrator_id, created_at DESC);

CREATE INDEX idx_outbox_operator_action_created ON outbox_operator_action USING btree (created_at, id);

CREATE INDEX idx_outbox_operator_action_event ON outbox_operator_action USING btree (outbox_event_id, created_at DESC);

CREATE TABLE maintenance_job_status (
    job_name text NOT NULL,
    run_id uuid NOT NULL,
    status text NOT NULL,
    started_at timestamp with time zone NOT NULL,
    finished_at timestamp with time zone,
    last_succeeded_at timestamp with time zone,
    duration_ms integer,
    processed_count bigint DEFAULT 0 NOT NULL,
    batch_count integer DEFAULT 0 NOT NULL,
    work_remaining boolean DEFAULT false NOT NULL,
    last_error_code text,
    updated_at timestamp with time zone DEFAULT clock_timestamp() NOT NULL,
    CONSTRAINT chk_maintenance_job_error CHECK ((((status = 'failed'::text) AND (last_error_code IS NOT NULL)) OR ((status <> 'failed'::text) AND (last_error_code IS NULL)))),
    CONSTRAINT chk_maintenance_job_finished CHECK ((((status = 'running'::text) AND (finished_at IS NULL) AND (duration_ms IS NULL)) OR ((status <> 'running'::text) AND (finished_at IS NOT NULL) AND (duration_ms IS NOT NULL)))),
    CONSTRAINT maintenance_job_status_duration_ms_check CHECK (((duration_ms IS NULL) OR ((duration_ms >= 0) AND (duration_ms <= 86400000)))),
    CONSTRAINT maintenance_job_status_batch_count_check CHECK ((batch_count >= 0)),
    CONSTRAINT maintenance_job_status_job_name_check CHECK ((job_name = ANY (ARRAY['matches'::text, 'photos'::text, 'privacy'::text, 'outbox'::text, 'billing'::text]))),
    CONSTRAINT maintenance_job_status_last_error_code_check CHECK (((last_error_code IS NULL) OR (last_error_code ~ '^[a-z][a-z0-9_]{0,63}$'::text))),
    CONSTRAINT maintenance_job_status_processed_count_check CHECK ((processed_count >= 0)),
    CONSTRAINT maintenance_job_status_status_check CHECK ((status = ANY (ARRAY['running'::text, 'succeeded'::text, 'failed'::text, 'skipped'::text]))),
    CONSTRAINT maintenance_job_status_pkey PRIMARY KEY (job_name)
);

COMMENT ON TABLE maintenance_job_status IS 'Last bounded execution outcome for each in-process maintenance job.';

COMMENT ON COLUMN maintenance_job_status.batch_count IS 'Number of bounded batches committed by the latest run.';

COMMENT ON COLUMN maintenance_job_status.work_remaining IS 'True when the latest bounded run exhausted its work budget and should be resumed.';

-- Droits des personnes et audit

CREATE TABLE data_subject_request (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    user_id uuid NOT NULL,
    type text NOT NULL,
    status text DEFAULT 'pending'::text NOT NULL,
    requested_at timestamp with time zone DEFAULT now() NOT NULL,
    completed_at timestamp with time zone,
    handled_by uuid,
    notes text,
    CONSTRAINT data_subject_request_status_check CHECK ((status = ANY (ARRAY['pending'::text, 'in_progress'::text, 'completed'::text, 'rejected'::text]))),
    CONSTRAINT data_subject_request_type_check CHECK ((type = ANY (ARRAY['access'::text, 'erasure'::text, 'portability'::text, 'rectification'::text, 'restriction'::text, 'objection'::text]))),
    CONSTRAINT data_subject_request_pkey PRIMARY KEY (id),
    CONSTRAINT data_subject_request_handled_by_fkey FOREIGN KEY (handled_by) REFERENCES user_account(user_id) ON DELETE SET NULL,
    CONSTRAINT data_subject_request_user_id_fkey FOREIGN KEY (user_id) REFERENCES user_account(user_id) ON DELETE CASCADE
);

CREATE INDEX idx_dsr_completed ON data_subject_request USING btree (completed_at, id) WHERE ((status = ANY (ARRAY['completed'::text, 'rejected'::text])) AND (completed_at IS NOT NULL));

CREATE UNIQUE INDEX idx_dsr_one_open_per_type ON data_subject_request USING btree (user_id, type) WHERE (status = ANY (ARRAY['pending'::text, 'in_progress'::text]));

CREATE INDEX idx_dsr_requested ON data_subject_request USING btree (requested_at, id);

CREATE INDEX idx_dsr_status ON data_subject_request USING btree (status, requested_at, id);

CREATE INDEX idx_dsr_user ON data_subject_request USING btree (user_id, requested_at DESC, id DESC);

CREATE TABLE account_erasure (
    request_id uuid NOT NULL,
    user_id uuid NOT NULL,
    step text DEFAULT 'stripe'::text NOT NULL,
    scylla_partition smallint DEFAULT 0 NOT NULL,
    created_at timestamp with time zone DEFAULT clock_timestamp() NOT NULL,
    updated_at timestamp with time zone DEFAULT clock_timestamp() NOT NULL,
    completed_at timestamp with time zone,
    CONSTRAINT account_erasure_check CHECK (((step = 'completed'::text) = (completed_at IS NOT NULL))),
    CONSTRAINT account_erasure_scylla_partition_check CHECK (((scylla_partition >= 0) AND (scylla_partition <= 64))),
    CONSTRAINT account_erasure_step_check CHECK ((step = ANY (ARRAY['stripe'::text, 'photos'::text, 'scylla'::text, 'postgres'::text, 'completed'::text]))),
    CONSTRAINT account_erasure_pkey PRIMARY KEY (request_id),
    CONSTRAINT account_erasure_user_id_key UNIQUE (user_id),
    CONSTRAINT account_erasure_request_id_fkey FOREIGN KEY (request_id) REFERENCES data_subject_request(id) ON DELETE CASCADE,
    CONSTRAINT account_erasure_user_id_fkey FOREIGN KEY (user_id) REFERENCES user_account(user_id) ON DELETE CASCADE
);

COMMENT ON TABLE account_erasure IS 'Resumable erasure checkpoints, retained with the parent DSR (existing retention). No provider payload or secret.';

CREATE TABLE account_deletion_token (
    id uuid NOT NULL,
    user_id uuid NOT NULL,
    token_hash text NOT NULL,
    expires_at timestamp with time zone NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT account_deletion_token_pkey PRIMARY KEY (id),
    CONSTRAINT account_deletion_token_user_id_key UNIQUE (user_id),
    CONSTRAINT account_deletion_token_user_id_fkey FOREIGN KEY (user_id) REFERENCES user_account(user_id) ON DELETE CASCADE
);

CREATE INDEX idx_account_deletion_token_expires ON account_deletion_token USING btree (expires_at, id);

CREATE TABLE data_access_log (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    accessed_user_id uuid NOT NULL,
    accessor_id uuid,
    accessor_role text,
    action text NOT NULL,
    reason text,
    accessed_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT data_access_log_action_check CHECK ((action = ANY (ARRAY['view_profile'::text, 'view_messages'::text, 'view_matches'::text, 'export_data'::text, 'admin_ban'::text, 'admin_unban'::text, 'admin_review_report'::text, 'admin_review_dsr'::text, 'admin_reconcile_photo'::text, 'view_moderation_content'::text, 'admin_review_content'::text, 'system_anonymize'::text, 'system_export_portability'::text]))),
    CONSTRAINT data_access_log_pkey PRIMARY KEY (id),
    CONSTRAINT data_access_log_accessor_id_fkey FOREIGN KEY (accessor_id) REFERENCES user_account(user_id) ON DELETE SET NULL
);

CREATE INDEX idx_dal_accessed_user ON data_access_log USING btree (accessed_user_id, accessed_at DESC, id DESC);

CREATE INDEX idx_dal_accessor ON data_access_log USING btree (accessor_id);

CREATE INDEX idx_dal_date ON data_access_log USING btree (accessed_at, id);

-- Authentification administrateur

CREATE TABLE admin_webauthn_credential (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    user_id uuid NOT NULL,
    credential_id text NOT NULL,
    public_key bytea NOT NULL,
    counter bigint DEFAULT 0 NOT NULL,
    device_type text NOT NULL,
    backed_up boolean NOT NULL,
    transports text[] DEFAULT ARRAY[]::text[] NOT NULL,
    aaguid uuid,
    name text NOT NULL,
    created_at timestamp with time zone DEFAULT clock_timestamp() NOT NULL,
    last_used_at timestamp with time zone,
    revoked_at timestamp with time zone,
    CONSTRAINT admin_webauthn_credential_counter_check CHECK (((counter >= 0) AND (counter <= '4294967295'::bigint))),
    CONSTRAINT admin_webauthn_credential_device_type_check CHECK ((device_type = ANY (ARRAY['singleDevice'::text, 'multiDevice'::text]))),
    CONSTRAINT chk_admin_webauthn_credential_id CHECK (char_length(credential_id) BETWEEN 1 AND 2048 AND credential_id ~ '^[A-Za-z0-9_-]+$'),
    CONSTRAINT chk_admin_webauthn_name CHECK (((name = btrim(name)) AND ((char_length(name) >= 1) AND (char_length(name) <= 100)) AND (octet_length(name) <= 200))),
    CONSTRAINT chk_admin_webauthn_public_key CHECK (((octet_length(public_key) >= 1) AND (octet_length(public_key) <= 8192))),
    CONSTRAINT chk_admin_webauthn_transports CHECK ((transports <@ ARRAY['ble'::text, 'cable'::text, 'hybrid'::text, 'internal'::text, 'nfc'::text, 'smart-card'::text, 'usb'::text])),
    CONSTRAINT admin_webauthn_credential_credential_id_key UNIQUE (credential_id),
    CONSTRAINT admin_webauthn_credential_pkey PRIMARY KEY (id),
    CONSTRAINT admin_webauthn_credential_user_id_fkey FOREIGN KEY (user_id) REFERENCES user_account(user_id) ON DELETE CASCADE
);

COMMENT ON TABLE admin_webauthn_credential IS 'Public WebAuthn credentials for administrators; private keys never leave authenticators.';

CREATE INDEX idx_admin_webauthn_credential_user ON admin_webauthn_credential USING btree (user_id, created_at DESC) WHERE (revoked_at IS NULL);

CREATE TABLE admin_session (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    user_id uuid NOT NULL,
    credential_id uuid NOT NULL,
    token_hash bytea NOT NULL,
    authenticated_at timestamp with time zone DEFAULT clock_timestamp() NOT NULL,
    last_seen_at timestamp with time zone DEFAULT clock_timestamp() NOT NULL,
    idle_expires_at timestamp with time zone NOT NULL,
    absolute_expires_at timestamp with time zone NOT NULL,
    revoked_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT clock_timestamp() NOT NULL,
    CONSTRAINT admin_session_check CHECK ((idle_expires_at > created_at)),
    CONSTRAINT admin_session_check1 CHECK ((absolute_expires_at >= idle_expires_at)),
    CONSTRAINT admin_session_token_hash_check CHECK ((octet_length(token_hash) = 32)),
    CONSTRAINT admin_session_pkey PRIMARY KEY (id),
    CONSTRAINT admin_session_token_hash_key UNIQUE (token_hash),
    CONSTRAINT admin_session_credential_id_fkey FOREIGN KEY (credential_id) REFERENCES admin_webauthn_credential(id) ON DELETE CASCADE,
    CONSTRAINT admin_session_user_id_fkey FOREIGN KEY (user_id) REFERENCES user_account(user_id) ON DELETE CASCADE
);

COMMENT ON TABLE admin_session IS 'Short, revocable administrator sessions. Only SHA-256 token hashes are persisted.';

CREATE INDEX idx_admin_session_expiry ON admin_session USING btree (absolute_expires_at, id);

CREATE INDEX idx_admin_session_revoked ON admin_session USING btree (revoked_at, id) WHERE (revoked_at IS NOT NULL);

CREATE INDEX idx_admin_session_user ON admin_session USING btree (user_id, created_at DESC) WHERE (revoked_at IS NULL);

CREATE TABLE admin_webauthn_bootstrap (
    id uuid NOT NULL,
    user_id uuid NOT NULL,
    secret_hash bytea NOT NULL,
    expires_at timestamp with time zone NOT NULL,
    consumed_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT clock_timestamp() NOT NULL,
    CONSTRAINT admin_webauthn_bootstrap_check CHECK ((expires_at > created_at)),
    CONSTRAINT admin_webauthn_bootstrap_secret_hash_check CHECK ((octet_length(secret_hash) = 32)),
    CONSTRAINT admin_webauthn_bootstrap_pkey PRIMARY KEY (id),
    CONSTRAINT admin_webauthn_bootstrap_user_id_fkey FOREIGN KEY (user_id) REFERENCES user_account(user_id) ON DELETE CASCADE
);

CREATE INDEX idx_admin_webauthn_bootstrap_consumed ON admin_webauthn_bootstrap USING btree (expires_at, id) WHERE (consumed_at IS NOT NULL);

CREATE INDEX idx_admin_webauthn_bootstrap_expiry ON admin_webauthn_bootstrap USING btree (expires_at) WHERE (consumed_at IS NULL);

CREATE UNIQUE INDEX uq_admin_webauthn_bootstrap_active_user ON admin_webauthn_bootstrap USING btree (user_id) WHERE (consumed_at IS NULL);

CREATE TABLE admin_webauthn_challenge (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    purpose text NOT NULL,
    challenge_hash bytea NOT NULL,
    user_id uuid,
    bootstrap_id uuid,
    expires_at timestamp with time zone NOT NULL,
    consumed_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT clock_timestamp() NOT NULL,
    CONSTRAINT admin_webauthn_challenge_challenge_hash_check CHECK ((octet_length(challenge_hash) = 32)),
    CONSTRAINT admin_webauthn_challenge_check CHECK ((expires_at > created_at)),
    CONSTRAINT admin_webauthn_challenge_purpose_check CHECK ((purpose = ANY (ARRAY['bootstrap_registration'::text, 'additional_registration'::text, 'authentication'::text]))),
    CONSTRAINT chk_admin_webauthn_challenge_owner CHECK ((((purpose = 'authentication'::text) AND (user_id IS NULL) AND (bootstrap_id IS NULL)) OR ((purpose = 'additional_registration'::text) AND (user_id IS NOT NULL) AND (bootstrap_id IS NULL)) OR ((purpose = 'bootstrap_registration'::text) AND (user_id IS NOT NULL) AND (bootstrap_id IS NOT NULL)))),
    CONSTRAINT admin_webauthn_challenge_pkey PRIMARY KEY (id),
    CONSTRAINT admin_webauthn_challenge_bootstrap_id_fkey FOREIGN KEY (bootstrap_id) REFERENCES admin_webauthn_bootstrap(id) ON DELETE CASCADE,
    CONSTRAINT admin_webauthn_challenge_user_id_fkey FOREIGN KEY (user_id) REFERENCES user_account(user_id) ON DELETE CASCADE
);

CREATE INDEX idx_admin_webauthn_challenge_consumed ON admin_webauthn_challenge USING btree (expires_at, id) WHERE (consumed_at IS NOT NULL);

CREATE INDEX idx_admin_webauthn_challenge_expiry ON admin_webauthn_challenge USING btree (expires_at) WHERE (consumed_at IS NULL);

CREATE TABLE admin_auth_event (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    user_id uuid NOT NULL,
    credential_id uuid,
    session_id uuid,
    event_type text NOT NULL,
    created_at timestamp with time zone DEFAULT clock_timestamp() NOT NULL,
    CONSTRAINT admin_auth_event_event_type_check CHECK ((event_type = ANY (ARRAY['bootstrap_issued'::text, 'bootstrap_registered'::text, 'login_succeeded'::text, 'credential_added'::text, 'credential_renamed'::text, 'credential_revoked'::text, 'session_revoked'::text, 'other_sessions_revoked'::text, 'logout'::text]))),
    CONSTRAINT admin_auth_event_pkey PRIMARY KEY (id),
    CONSTRAINT admin_auth_event_credential_id_fkey FOREIGN KEY (credential_id) REFERENCES admin_webauthn_credential(id) ON DELETE SET NULL,
    CONSTRAINT admin_auth_event_session_id_fkey FOREIGN KEY (session_id) REFERENCES admin_session(id) ON DELETE SET NULL,
    CONSTRAINT admin_auth_event_user_id_fkey FOREIGN KEY (user_id) REFERENCES user_account(user_id) ON DELETE CASCADE
);

CREATE INDEX idx_admin_auth_event_created ON admin_auth_event USING btree (created_at, id);

CREATE INDEX idx_admin_auth_event_user ON admin_auth_event USING btree (user_id, created_at DESC, id DESC);

-- Fonctions métier et guards transactionnels

CREATE FUNCTION fct_anonymize_user(p_user_id uuid) RETURNS void
    LANGUAGE plpgsql
    AS $$
BEGIN
  INSERT INTO account_tombstone (phone_number_hash, reason, expires_at)
  SELECT phone_number_hash, 'banned_account', now() + INTERVAL '3 years'
  FROM user_account
  WHERE user_id = p_user_id AND is_banned = true
  ON CONFLICT (phone_number_hash) DO UPDATE
    SET expires_at = GREATEST(account_tombstone.expires_at, EXCLUDED.expires_at);

  INSERT INTO data_access_log (accessed_user_id, accessor_role, action, reason)
  VALUES (p_user_id, 'system', 'system_anonymize', 'Account erasure');

  UPDATE match_init
  SET status = 'ended', purge_after = COALESCE(purge_after, now() + INTERVAL '30 days')
  WHERE (user1_id = p_user_id OR user2_id = p_user_id)
    AND status IN ('active', 'awaiting_continuation', 'confirmed');

  UPDATE chat_message SET content = '[Message supprimé]'
  WHERE sender_id = p_user_id;

  DELETE FROM user_profile WHERE user_id = p_user_id;
  DELETE FROM user_preferences WHERE user_id = p_user_id;
  DELETE FROM user_trait WHERE user_id = p_user_id;
  DELETE FROM user_presence WHERE user_id = p_user_id;
  DELETE FROM refresh_tokens WHERE user_id = p_user_id;
  DELETE FROM device_token WHERE user_id = p_user_id;
  DELETE FROM notification WHERE user_id = p_user_id;
  DELETE FROM user_block WHERE blocker_id = p_user_id OR blocked_id = p_user_id;
  DELETE FROM user_subscription WHERE user_id = p_user_id;
  DELETE FROM continuation_usage WHERE user_id = p_user_id;
  DELETE FROM match_state WHERE user_id = p_user_id;

  UPDATE user_consent SET
    withdrawn_at = COALESCE(withdrawn_at, now()),
    ip_address = NULL,
    user_agent = NULL
  WHERE user_id = p_user_id;

  UPDATE user_account SET
    phone_number_hash = 'anon_' || encode(gen_random_bytes(16), 'hex'),
    phone_number_encrypted = '',
    is_banned = false,
    banned_at = NULL,
    banned_reason = NULL,
    banned_by = NULL,
    deleted_at = COALESCE(deleted_at, now()),
    anonymized_at = now()
  WHERE user_id = p_user_id;
END;
$$;

CREATE FUNCTION fct_check_user_age() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
  IF NEW.birthdate > (CURRENT_DATE - INTERVAL '18 years')::date THEN
    RAISE EXCEPTION 'User must be at least 18 years old';
  END IF;

  RETURN NEW;
END;
$$;

CREATE FUNCTION fct_cleanup_billing_identity() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
  UPDATE billing_invoice SET user_id = NULL WHERE user_id = NEW.user_id;
  DELETE FROM billing_checkout_session WHERE user_id = NEW.user_id;
  DELETE FROM billing_customer WHERE user_id = NEW.user_id;
  RETURN NEW;
END;
$$;

CREATE FUNCTION fct_erase_mobile_sessions() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
  DELETE FROM refresh_token_family WHERE user_id = NEW.user_id;
  RETURN NEW;
END;
$$;

CREATE FUNCTION fct_erase_notifications() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
  DELETE FROM notification WHERE user_id = NEW.user_id;
  RETURN NEW;
END;
$$;

CREATE FUNCTION fct_require_live_account() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE
  account_ids UUID[] := '{}';
  column_name TEXT;
  account_row RECORD;
BEGIN
  FOREACH column_name IN ARRAY TG_ARGV LOOP
    account_ids := array_append(account_ids, (to_jsonb(NEW)->>column_name)::uuid);
  END LOOP;
  FOR account_row IN
    SELECT user_id, deleted_at FROM user_account WHERE user_id = ANY(account_ids)
    ORDER BY user_id FOR SHARE
  LOOP
    IF account_row.deleted_at IS NOT NULL THEN
      RAISE EXCEPTION USING ERRCODE = 'P0E01', MESSAGE = 'account_unavailable';
    END IF;
  END LOOP;
  RETURN NEW;
END;
$$;

CREATE FUNCTION fct_require_live_match() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE participant RECORD;
BEGIN
  FOR participant IN
    SELECT account.user_id, account.deleted_at FROM user_account account
    JOIN match_init m ON account.user_id IN (m.user1_id, m.user2_id)
    WHERE m.id = NEW.match_id ORDER BY account.user_id FOR SHARE OF account
  LOOP
    IF participant.deleted_at IS NOT NULL THEN
      RAISE EXCEPTION USING ERRCODE = 'P0E01', MESSAGE = 'account_unavailable';
    END IF;
  END LOOP;
  RETURN NEW;
END;
$$;

-- Triggers (installés après toutes les tables et fonctions)

CREATE TRIGGER trg_cleanup_billing_identity AFTER UPDATE OF anonymized_at ON user_account FOR EACH ROW WHEN (((old.anonymized_at IS DISTINCT FROM new.anonymized_at) AND (new.anonymized_at IS NOT NULL))) EXECUTE FUNCTION fct_cleanup_billing_identity();

CREATE TRIGGER trg_enforce_user_age BEFORE INSERT OR UPDATE ON user_profile FOR EACH ROW EXECUTE FUNCTION fct_check_user_age();

CREATE TRIGGER trg_erase_mobile_sessions AFTER UPDATE OF deleted_at ON user_account FOR EACH ROW WHEN ((new.deleted_at IS NOT NULL)) EXECUTE FUNCTION fct_erase_mobile_sessions();

CREATE TRIGGER trg_erase_notifications AFTER UPDATE OF deleted_at ON user_account FOR EACH ROW WHEN ((new.deleted_at IS NOT NULL)) EXECUTE FUNCTION fct_erase_notifications();

CREATE TRIGGER trg_live_answer BEFORE INSERT OR UPDATE ON user_profile_answer FOR EACH ROW EXECUTE FUNCTION fct_require_live_account('user_id');

CREATE TRIGGER trg_live_billing_customer BEFORE INSERT OR UPDATE ON billing_customer FOR EACH ROW WHEN ((new.stripe_customer_deleted_at IS NULL)) EXECUTE FUNCTION fct_require_live_account('user_id');

CREATE TRIGGER trg_live_block BEFORE INSERT ON user_block FOR EACH ROW EXECUTE FUNCTION fct_require_live_account('blocker_id', 'blocked_id');

CREATE TRIGGER trg_live_checkout BEFORE INSERT ON billing_checkout_session FOR EACH ROW EXECUTE FUNCTION fct_require_live_account('user_id');

CREATE TRIGGER trg_live_consent BEFORE INSERT ON user_consent FOR EACH ROW EXECUTE FUNCTION fct_require_live_account('user_id');

CREATE TRIGGER trg_live_device BEFORE INSERT OR UPDATE ON device_token FOR EACH ROW EXECUTE FUNCTION fct_require_live_account('user_id');

CREATE TRIGGER trg_live_dsr BEFORE INSERT ON data_subject_request FOR EACH ROW EXECUTE FUNCTION fct_require_live_account('user_id');

CREATE TRIGGER trg_live_match BEFORE INSERT ON match_init FOR EACH ROW EXECUTE FUNCTION fct_require_live_account('user1_id', 'user2_id');

CREATE TRIGGER trg_live_match_state BEFORE INSERT OR UPDATE ON match_state FOR EACH ROW EXECUTE FUNCTION fct_require_live_match();

CREATE TRIGGER trg_live_message BEFORE INSERT ON chat_message FOR EACH ROW EXECUTE FUNCTION fct_require_live_match();

CREATE TRIGGER trg_live_moderation BEFORE INSERT OR UPDATE ON content_moderation_case FOR EACH ROW EXECUTE FUNCTION fct_require_live_account('user_id');

CREATE TRIGGER trg_live_notification BEFORE INSERT ON notification FOR EACH ROW EXECUTE FUNCTION fct_require_live_account('user_id');

CREATE TRIGGER trg_live_photo BEFORE INSERT OR UPDATE ON user_photo FOR EACH ROW WHEN ((new.status <> 'deleting'::text)) EXECUTE FUNCTION fct_require_live_account('user_id');

CREATE TRIGGER trg_live_photo_request BEFORE INSERT ON photo_upload_request FOR EACH ROW EXECUTE FUNCTION fct_require_live_account('user_id');

CREATE TRIGGER trg_live_preferences BEFORE INSERT OR UPDATE ON user_preferences FOR EACH ROW EXECUTE FUNCTION fct_require_live_account('user_id');

CREATE TRIGGER trg_live_presence BEFORE INSERT OR UPDATE OF user_id, latitude, longitude, updated_at ON user_presence FOR EACH ROW EXECUTE FUNCTION fct_require_live_account('user_id');

CREATE TRIGGER trg_live_presence_freshness BEFORE UPDATE OF is_location_fresh ON user_presence FOR EACH ROW WHEN (new.is_location_fresh) EXECUTE FUNCTION fct_require_live_account('user_id');

CREATE TRIGGER trg_live_profile BEFORE INSERT OR UPDATE ON user_profile FOR EACH ROW EXECUTE FUNCTION fct_require_live_account('user_id');

CREATE TRIGGER trg_live_report BEFORE INSERT ON user_report FOR EACH ROW EXECUTE FUNCTION fct_require_live_account('reporter_id');

CREATE TRIGGER trg_live_subscription BEFORE INSERT OR UPDATE ON user_subscription FOR EACH ROW EXECUTE FUNCTION fct_require_live_account('user_id');

CREATE TRIGGER trg_live_trait BEFORE INSERT OR UPDATE ON user_trait FOR EACH ROW EXECUTE FUNCTION fct_require_live_account('user_id');
