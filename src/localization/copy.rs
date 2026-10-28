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
        (Language::Fr, MatchExpiring) => (
            "Conversation inactive",
            "Une conversation sera supprimée dans les 7 prochains jours sans nouveau message.",
        ),
        (Language::En, MatchExpiring) => (
            "Inactive conversation",
            "A conversation will be deleted within 7 days unless a new message is sent.",
        ),
        (Language::Es, MatchExpiring) => (
            "Conversación inactiva",
            "Una conversación se eliminará en los próximos 7 días si no se envía un nuevo mensaje.",
        ),
        (Language::It, MatchExpiring) => (
            "Conversazione inattiva",
            "Una conversazione verrà eliminata entro 7 giorni senza un nuovo messaggio.",
        ),
        (Language::Fr, NewMatch) => ("Nouveau match", "Vous avez un nouveau match sur Histae."),
        (Language::Fr, NewMessage) => (
            "Nouveau message",
            "Vous avez reçu un nouveau message sur Histae.",
        ),
        (Language::Fr, BillingPaymentFailed) => (
            "Paiement à vérifier",
            "Votre paiement Premium a échoué. Vérifiez votre moyen de paiement.",
        ),
        (Language::En, NewMatch) => ("New match", "You have a new match on Histae."),
        (Language::En, NewMessage) => ("New message", "You have received a new message on Histae."),
        (Language::En, BillingPaymentFailed) => (
            "Payment needs attention",
            "Your Premium payment failed. Please check your payment method.",
        ),
        (Language::Es, NewMatch) => ("Nuevo match", "Tienes un nuevo match en Histae."),
        (Language::Es, NewMessage) => ("Nuevo mensaje", "Has recibido un nuevo mensaje en Histae."),
        (Language::Es, BillingPaymentFailed) => (
            "Revisa tu pago",
            "Tu pago Premium ha fallado. Revisa tu método de pago.",
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
