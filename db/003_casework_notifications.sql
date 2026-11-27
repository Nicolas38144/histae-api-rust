CREATE OR REPLACE FUNCTION public.fct_assisted_export_is_available(p_request_id uuid, p_grant_id uuid)
RETURNS boolean LANGUAGE sql SECURITY DEFINER
SET search_path = pg_catalog, public, pg_temp
AS $$
  SELECT EXISTS (
    SELECT 1 FROM assisted_data_export AS grant_record
    JOIN data_subject_request AS request ON request.id = grant_record.request_id
    WHERE grant_record.id = p_grant_id AND request.id = p_request_id
      AND grant_record.expires_at > clock_timestamp()
      AND request.type IN ('access', 'portability')
      AND request.status IN ('in_progress', 'completed')
  )
$$;
REVOKE EXECUTE ON FUNCTION public.fct_assisted_export_is_available(uuid, uuid) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION public.fct_assisted_export_is_available(uuid, uuid) TO histae_outbox, histae_maintenance;

CREATE OR REPLACE FUNCTION public.fct_support_case_update_is_current(p_case_id uuid, p_user_id uuid, p_updated_at timestamptz)
RETURNS boolean LANGUAGE sql SECURITY DEFINER
SET search_path = pg_catalog, public, pg_temp
AS $$
  SELECT EXISTS (
    SELECT 1 FROM support_case
    WHERE id = p_case_id AND user_id = p_user_id AND updated_at = p_updated_at
      AND expires_at > clock_timestamp() AND status IN ('in_progress', 'closed')
  )
$$;
REVOKE EXECUTE ON FUNCTION public.fct_support_case_update_is_current(uuid, uuid, timestamptz) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION public.fct_support_case_update_is_current(uuid, uuid, timestamptz) TO histae_outbox, histae_maintenance;

ALTER TABLE public.notification
    DROP CONSTRAINT notification_type_check,
    ADD CONSTRAINT notification_type_check CHECK (type IN (
        'new_match', 'new_message', 'profile_liked', 'match_expiring',
        'continuation_requested', 'conversation_continued', 'photos_revealed',
        'first_period_ending', 'data_request_replied', 'new_session', 'support_case_updated'
    ));
