-- The ordinary API appends events. Their independent archive is delivered
-- transactionally through a queue the application cannot read or change.
CREATE TABLE IF NOT EXISTS public.audit_archive_pending (
  sequence_id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  source text NOT NULL CHECK (source IN ('data_access_log', 'admin_auth_event')),
  event_id uuid NOT NULL,
  occurred_at timestamptz NOT NULL,
  event jsonb NOT NULL,
  UNIQUE (source, event_id)
);

CREATE INDEX IF NOT EXISTS idx_admin_auth_event_retention ON public.admin_auth_event (created_at, id);

-- Keep the original IDs for the existing retention window even after an
-- account, passkey or session is erased. Cascades would bypass table grants.
ALTER TABLE public.data_access_log DROP CONSTRAINT IF EXISTS data_access_log_accessor_id_fkey;
ALTER TABLE public.admin_auth_event
  DROP CONSTRAINT IF EXISTS admin_auth_event_credential_id_fkey,
  DROP CONSTRAINT IF EXISTS admin_auth_event_session_id_fkey,
  DROP CONSTRAINT IF EXISTS admin_auth_event_user_id_fkey;

CREATE OR REPLACE FUNCTION public.enqueue_audit_archive() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog AS $$
DECLARE event_time timestamptz;
BEGIN
  IF TG_TABLE_NAME = 'data_access_log' THEN
    event_time := NEW.accessed_at;
  ELSE
    event_time := NEW.created_at;
  END IF;
  INSERT INTO public.audit_archive_pending (source, event_id, occurred_at, event)
    VALUES (TG_TABLE_NAME, NEW.id, event_time, to_jsonb(NEW));
  RETURN NEW;
END $$;

-- Install the capture trigger and queue pre-existing events exactly once.
DO $$ BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_trigger WHERE tgrelid = 'public.data_access_log'::regclass AND tgname = 'archive_audit_event') THEN
    CREATE TRIGGER archive_audit_event AFTER INSERT ON public.data_access_log
      FOR EACH ROW EXECUTE FUNCTION public.enqueue_audit_archive();
    INSERT INTO public.audit_archive_pending (source, event_id, occurred_at, event)
      SELECT 'data_access_log', id, accessed_at, to_jsonb(entry) FROM public.data_access_log entry
      ON CONFLICT (source, event_id) DO NOTHING;
  END IF;
  IF NOT EXISTS (SELECT 1 FROM pg_trigger WHERE tgrelid = 'public.admin_auth_event'::regclass AND tgname = 'archive_audit_event') THEN
    CREATE TRIGGER archive_audit_event AFTER INSERT ON public.admin_auth_event
      FOR EACH ROW EXECUTE FUNCTION public.enqueue_audit_archive();
    INSERT INTO public.audit_archive_pending (source, event_id, occurred_at, event)
      SELECT 'admin_auth_event', id, created_at, to_jsonb(entry) FROM public.admin_auth_event entry
      ON CONFLICT (source, event_id) DO NOTHING;
  END IF;
END $$;

CREATE OR REPLACE FUNCTION public.purge_archived_audit_events(batch_size integer) RETURNS bigint
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog AS $$
DECLARE
  cutoff timestamptz := statement_timestamp() - interval '1 year';
  batch_limit integer := greatest(0, least(coalesce(batch_size, 0), 1000));
  access_removed bigint;
  auth_removed bigint;
BEGIN
  DELETE FROM public.data_access_log WHERE id IN (
    SELECT entry.id FROM public.data_access_log entry WHERE entry.accessed_at < cutoff
      AND NOT EXISTS (SELECT 1 FROM public.audit_archive_pending pending
        WHERE pending.source = 'data_access_log' AND pending.event_id = entry.id)
    ORDER BY entry.accessed_at, entry.id LIMIT batch_limit FOR UPDATE SKIP LOCKED);
  GET DIAGNOSTICS access_removed = ROW_COUNT;
  DELETE FROM public.admin_auth_event WHERE id IN (
    SELECT entry.id FROM public.admin_auth_event entry WHERE entry.created_at < cutoff
      AND NOT EXISTS (SELECT 1 FROM public.audit_archive_pending pending
        WHERE pending.source = 'admin_auth_event' AND pending.event_id = entry.id)
    ORDER BY entry.created_at, entry.id LIMIT batch_limit FOR UPDATE SKIP LOCKED);
  GET DIAGNOSTICS auth_removed = ROW_COUNT;
  RETURN access_removed + auth_removed;
END $$;

REVOKE ALL ON public.audit_archive_pending FROM PUBLIC;
REVOKE ALL ON FUNCTION public.enqueue_audit_archive(), public.purge_archived_audit_events(integer) FROM PUBLIC;
DO $$ BEGIN
  IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'histae_app') THEN
    REVOKE ALL ON public.data_access_log, public.admin_auth_event, public.audit_archive_pending FROM histae_app;
    GRANT SELECT, INSERT ON public.data_access_log, public.admin_auth_event TO histae_app;
    REVOKE ALL ON SEQUENCE public.audit_archive_pending_sequence_id_seq FROM histae_app;
    REVOKE ALL ON FUNCTION public.enqueue_audit_archive(), public.purge_archived_audit_events(integer) FROM histae_app;
  END IF;
  IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'histae_audit_archive') THEN
    GRANT SELECT, DELETE ON public.audit_archive_pending TO histae_audit_archive;
    GRANT EXECUTE ON FUNCTION public.purge_archived_audit_events(integer) TO histae_audit_archive;
  END IF;
END $$;
