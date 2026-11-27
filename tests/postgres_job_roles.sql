-- Run against disposable databases through each job's own TCP login.
BEGIN;
DO $$
DECLARE
  statement text;
  forbidden text[] := ARRAY[
    'SET ROLE histae_admin',
    'SET ROLE histae_migrate',
    'SET ROLE histae_app',
    'SET ROLE histae_phone_sync',
    'CREATE TEMP TABLE forbidden_role_probe (id integer)',
    'SELECT phone_number_encrypted FROM user_account',
    'UPDATE user_account SET role = ''superadmin'' WHERE false',
    'SELECT content FROM chat_message',
    'SELECT otp_hash FROM otp_verification',
    'SELECT rotation_result FROM refresh_tokens',
    'SELECT * FROM admin_webauthn_credential',
    'SELECT * FROM billing_store_finance',
    'SELECT body, response FROM support_case',
    'SELECT id FROM assisted_data_export',
    'SELECT fct_anonymize_user(NULL)',
    'INSERT INTO account_erasure DEFAULT VALUES',
    'INSERT INTO outbox_event (event_type, aggregate_id) VALUES (''account.erase'', uuidv7())',
    'UPDATE sms_send_control SET suspended = false WHERE false'
  ];
BEGIN
  CASE current_user
    WHEN 'histae_outbox' THEN
      forbidden := forbidden || ARRAY[
        'SET ROLE histae_maintenance',
        'SELECT fct_run_privacy_retention(clock_timestamp(), 1)',
        'SELECT fct_schedule_store_reconciliation()',
        'UPDATE data_subject_request SET status = ''completed'' WHERE false',
        'UPDATE match_init SET status = ''confirmed'' WHERE false'
      ];
      IF fct_complete_claimed_erasure(uuidv7(), uuidv7(), uuidv7()) THEN
        RAISE EXCEPTION 'erasure must require a claimed request';
      END IF;
    WHEN 'histae_maintenance' THEN
      forbidden := forbidden || ARRAY[
        'SET ROLE histae_outbox',
        'SELECT fct_pending_web_otp(NULL)',
        'SELECT fct_complete_claimed_erasure(NULL, NULL, NULL)',
        'SELECT encrypted_phone FROM mobile_otp_request',
        'SELECT token FROM device_token',
        'SELECT reference_encrypted FROM billing_store_subscription',
        'DELETE FROM admin_session WHERE false',
        'DELETE FROM outbox_event WHERE false',
        'UPDATE outbox_event SET locked_by = NULL WHERE false',
        'UPDATE outbox_event SET status = ''pending'' WHERE false',
        'INSERT INTO notification (user_id, type) VALUES (uuidv7(), ''new_session'')'
      ];
    ELSE
      RAISE EXCEPTION 'incorrect job verification role';
  END CASE;

  IF fct_assisted_export_is_available(uuidv7(), uuidv7())
     OR fct_support_case_update_is_current(uuidv7(), uuidv7(), clock_timestamp()) THEN
    RAISE EXCEPTION 'notification checks must require the referenced records';
  END IF;

  FOREACH statement IN ARRAY forbidden LOOP
    BEGIN
      EXECUTE statement;
      RAISE EXCEPTION 'a forbidden operation succeeded: %', statement;
    EXCEPTION WHEN insufficient_privilege THEN
      NULL;
    END;
  END LOOP;
END;
$$;
ROLLBACK;
