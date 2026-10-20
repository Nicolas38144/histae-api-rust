/// Scheduling and delivery must evaluate the same billing relevance predicate.
/// The candidate notification is always bound to the SQL alias `n`.
pub const BILLING_NOTIFICATION_ELIGIBLE_SQL: &str = r#"
  (n.type = 'billing_payment_failed' AND EXISTS (
    SELECT 1 FROM billing_invoice invoice
    WHERE invoice.stripe_invoice_id = n.billing_reference AND invoice.user_id = n.user_id
      AND invoice.status = 'open' AND invoice.amount_remaining > 0
  ))
"#;
