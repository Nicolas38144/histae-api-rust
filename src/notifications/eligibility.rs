/// Scheduling and delivery must evaluate the same billing relevance predicate.
/// The candidate notification is always bound to the SQL alias `n`.
pub const BILLING_NOTIFICATION_ELIGIBLE_SQL: &str = r#"
  (n.type = 'billing_payment_failed' AND EXISTS (
    SELECT 1 FROM billing_invoice invoice
    WHERE invoice.stripe_invoice_id = n.billing_reference AND invoice.user_id = n.user_id
      AND invoice.status = 'open' AND invoice.amount_remaining > 0
  ))
"#;

/// Account-only events are rechecked both at scheduling and delivery.
pub const ACCOUNT_NOTIFICATION_ELIGIBLE_SQL: &str = r#"
  (n.type = 'data_request_replied' AND EXISTS (
    SELECT 1 FROM data_subject_request request
    WHERE request.id=(n.payload->>'request_id')::uuid AND request.user_id=n.user_id
      AND request.status=(n.payload->>'response_status')
      AND request.status IN ('in_progress','completed','rejected')
      AND nullif(btrim(request.response),'') IS NOT NULL
  )) OR (n.type = 'new_session' AND EXISTS (
    SELECT 1 FROM refresh_token_family opened_session
    WHERE opened_session.id=(n.payload->>'session_id')::uuid
      AND opened_session.user_id=n.user_id AND opened_session.revoked_at IS NULL
      AND opened_session.expires_at>clock_timestamp()
  ) AND EXISTS (
    SELECT 1 FROM device_token previous
    LEFT JOIN refresh_token_family previous_session ON previous_session.id=previous.session_id
    WHERE previous.user_id=n.user_id AND previous.session_id IS DISTINCT FROM (n.payload->>'session_id')::uuid
      AND (previous.session_id IS NULL OR (previous_session.revoked_at IS NULL AND previous_session.expires_at>clock_timestamp()))
  ))
"#;
/// Both enqueue and delivery bind the aliases n, preference and mute.
/// Inbox/realtime events are independent of these push-only preferences.
pub const PUSH_PREFERENCES_ELIGIBLE_SQL: &str = "
    CASE n.type
      WHEN 'new_message' THEN COALESCE(preference.messages,true)
      WHEN 'new_match' THEN COALESCE(preference.new_matches,true)
      WHEN 'continuation_requested' THEN COALESCE(preference.continuations,true)
      WHEN 'conversation_continued' THEN COALESCE(preference.continuations,true)
      WHEN 'photos_revealed' THEN COALESCE(preference.photos,true)
      WHEN 'first_period_ending' THEN COALESCE(preference.reminders,true)
      WHEN 'match_expiring' THEN COALESCE(preference.reminders,true)
      ELSE COALESCE(preference.account_security,true) END
    AND NOT COALESCE(mute.muted AND
      (mute.muted_until IS NULL OR mute.muted_until>clock_timestamp()),false)";

