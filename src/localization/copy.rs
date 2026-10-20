use super::Language;
use crate::notifications::domain::NotificationType;

pub fn otp(language: Language, code: &str, minutes: u64) -> String {
    match language {
        Language::Fr => format!(
            "Histae : votre code de verification est {code}. Il expire dans {minutes} minutes."
        ),
        Language::En => {
            format!("Histae: your verification code is {code}. It expires in {minutes} minutes.")
        }
        Language::Es => {
            format!("Histae: tu codigo de verificacion es {code}. Caduca en {minutes} minutos.")
        }
        Language::It => {
            format!("Histae: il tuo codice di verifica e {code}. Scade tra {minutes} minuti.")
        }
    }
}

pub const fn notification(
    language: Language,
    kind: NotificationType,
) -> (&'static str, &'static str) {
    use NotificationType::*;
    match (language, kind) {
        (Language::Fr, NewMatch) => ("Nouveau match", "Vous avez un nouveau match sur Histae."),
        (Language::Fr, NewMessage) => (
            "Nouveau message",
            "Vous avez reçu un nouveau message sur Histae.",
        ),
        (Language::Fr, BillingPaymentFailed) => (
            "Paiement à vérifier",
            "Votre paiement Premium a échoué. Vérifiez votre moyen de paiement.",
        ),
        (Language::Fr, SubscriptionTrialEnding) => (
            "Essai Premium",
            "Votre période d’essai Premium se termine bientôt.",
        ),
        (Language::En, NewMatch) => ("New match", "You have a new match on Histae."),
        (Language::En, NewMessage) => ("New message", "You have received a new message on Histae."),
        (Language::En, BillingPaymentFailed) => (
            "Payment needs attention",
            "Your Premium payment failed. Please check your payment method.",
        ),
        (Language::En, SubscriptionTrialEnding) => {
            ("Premium trial", "Your Premium trial is ending soon.")
        }
        (Language::Es, NewMatch) => ("Nuevo match", "Tienes un nuevo match en Histae."),
        (Language::Es, NewMessage) => ("Nuevo mensaje", "Has recibido un nuevo mensaje en Histae."),
        (Language::Es, BillingPaymentFailed) => (
            "Revisa tu pago",
            "Tu pago Premium ha fallado. Revisa tu método de pago.",
        ),
        (Language::Es, SubscriptionTrialEnding) => (
            "Prueba Premium",
            "Tu periodo de prueba Premium termina pronto.",
        ),
        (Language::It, NewMatch) => ("Nuovo match", "Hai un nuovo match su Histae."),
        (Language::It, NewMessage) => (
            "Nuovo messaggio",
            "Hai ricevuto un nuovo messaggio su Histae.",
        ),
        (Language::It, BillingPaymentFailed) => (
            "Pagamento da verificare",
            "Il pagamento Premium non è riuscito. Controlla il metodo di pagamento.",
        ),
        (Language::It, SubscriptionTrialEnding) => (
            "Prova Premium",
            "Il tuo periodo di prova Premium sta per terminare.",
        ),
    }
}

pub fn offer(language: Language, plan: &str, annual: bool) -> String {
    let period = match (language, annual) {
        (Language::Fr, false) => "Mensuel",
        (Language::Fr, true) => "Annuel",
        (Language::En, false) => "Monthly",
        (Language::En, true) => "Annual",
        (Language::Es, false) => "Mensual",
        (Language::Es, true) => "Anual",
        (Language::It, false) => "Mensile",
        (Language::It, true) => "Annuale",
    };
    format!("{plan} {period}")
}
