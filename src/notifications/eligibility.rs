/// Scheduling and delivery must evaluate the same billing relevance predicate.
/// The candidate notification is always bound to the SQL alias `n`.
pub const BILLING_NOTIFICATION_ELIGIBLE_SQL: &str = r#"
  (n.type = 'billing_payment_failed' AND EXISTS (
    SELECT 1 FROM billing_invoice invoice
    WHERE invoice.stripe_invoice_id = n.billing_reference AND invoice.user_id = n.user_id
      AND invoice.status = 'open' AND invoice.amount_remaining > 0
  )) OR (n.type = 'subscription_trial_ending' AND EXISTS (
    SELECT 1 FROM user_subscription subscription
    WHERE subscription.user_id = n.user_id AND subscription.provider = 'stripe'
      AND subscription.provider_subscription_id = n.billing_reference
      AND subscription.status = 'trialing'
      AND subscription.trial_ends_at = n.billing_trial_ends_at
      AND subscription.trial_ends_at > clock_timestamp()
  ))
"#;
