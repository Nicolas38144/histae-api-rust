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
        (Language::Fr, ContinuationRequested) => ("Continuer cette conversation ?", "Une personne souhaite poursuivre votre conversation. Et vous ?"),
        (Language::Fr, ConversationContinued) => ("Votre conversation continue", "Vous avez tous les deux choisi de poursuivre votre conversation."),
        (Language::Fr, PhotosRevealed) => ("Vos photos sont dévoilées", "Vous avez tous les deux accepté de partager vos photos."),
        (Language::Fr, FirstPeriodEnding) => ("La première période se termine bientôt", "Votre première période se termine dans moins de deux heures. Vous pourrez ensuite choisir de continuer."),
        (Language::Fr, DataRequestReplied) => ("Une réponse à votre démarche", "Une réponse est disponible dans Mes démarches."),
        (Language::Fr, NewSession) => ("Nouvelle connexion à votre compte", "Une nouvelle session Histae a été ouverte. Consultez vos sessions si ce n’était pas vous."),
        (Language::En, ContinuationRequested) => ("Continue this conversation?", "Someone would like to continue your conversation. Would you?"),
        (Language::En, ConversationContinued) => ("Your conversation continues", "You both chose to continue your conversation."),
        (Language::En, PhotosRevealed) => ("Your photos are now shared", "You both agreed to share your photos."),
        (Language::En, FirstPeriodEnding) => ("Your first period ends soon", "Your first period ends in less than two hours. You can then choose whether to continue."),
        (Language::En, DataRequestReplied) => ("A reply to your request", "A reply is available in My requests."),
        (Language::En, NewSession) => ("New sign-in to your account", "A new Histae session was opened. Check your sessions if this was not you."),
        (Language::Es, ContinuationRequested) => ("¿Continuar esta conversación?", "Una persona quiere continuar la conversación. ¿Y tú?"),
        (Language::Es, ConversationContinued) => ("La conversación continúa", "Ambos habéis elegido continuar la conversación."),
        (Language::Es, PhotosRevealed) => ("Ya podéis ver vuestras fotos", "Ambos habéis aceptado compartir vuestras fotos."),
        (Language::Es, FirstPeriodEnding) => ("El primer periodo termina pronto", "El primer periodo termina en menos de dos horas. Después podrás decidir si quieres continuar."),
        (Language::Es, DataRequestReplied) => ("Una respuesta a tu solicitud", "Hay una respuesta disponible en Mis solicitudes."),
        (Language::Es, NewSession) => ("Nuevo acceso a tu cuenta", "Se ha abierto una nueva sesión de Histae. Revisa tus sesiones si no has sido tú."),
        (Language::It, ContinuationRequested) => ("Continuare questa conversazione?", "Una persona desidera continuare la conversazione. E tu?"),
        (Language::It, ConversationContinued) => ("La vostra conversazione continua", "Avete entrambi scelto di continuare la conversazione."),
        (Language::It, PhotosRevealed) => ("Ora potete vedere le vostre foto", "Avete entrambi accettato di condividere le vostre foto."),
        (Language::It, FirstPeriodEnding) => ("Il primo periodo sta per terminare", "Il primo periodo termina tra meno di due ore. Poi potrai scegliere se continuare."),
        (Language::It, DataRequestReplied) => ("Una risposta alla tua richiesta", "È disponibile una risposta in Le mie richieste."),
        (Language::It, NewSession) => ("Nuovo accesso al tuo account", "È stata aperta una nuova sessione Histae. Controlla le tue sessioni se non sei stato tu."),

    }
}

/// Names are display-only: never copied to routing data or persisted notification payloads.
pub fn personalized_notification(
    language: Language,
    kind: NotificationType,
    firstname: Option<&str>,
) -> (&'static str, String) {
    let (title, fallback) = notification(language, kind);
    let Some(raw) = firstname else {
        return (title, fallback.to_owned());
    };
    let clean: String = raw
        .chars()
        .take(100)
        .map(|c| {
            if c.is_control() || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') {
                ' '
            } else {
                c
            }
        })
        .collect();
    let name = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() {
        return (title, fallback.to_owned());
    }
    use NotificationType::*;
    let body = match (language, kind) {
        (Language::Fr, NewMatch) => format!("Vous avez un nouveau match avec {name}."),
        (Language::Fr, NewMessage) => format!("{name} vous a envoyé un message."),
        (Language::Fr, MatchExpiring) => format!("Votre conversation avec {name} sera supprimée dans les 7 prochains jours sans nouveau message."),
        (Language::Fr, ContinuationRequested) => format!("{name} souhaite continuer, et vous ?"),
        (Language::Fr, ConversationContinued) => format!("{name} et vous avez choisi de continuer. Vous pouvez reprendre votre conversation."),
        (Language::Fr, PhotosRevealed) => format!("{name} et vous partagez désormais vos photos."),
        (Language::Fr, FirstPeriodEnding) => format!("Votre première période avec {name} se termine dans moins de deux heures. Vous pourrez ensuite choisir de continuer."),
        (Language::En, NewMatch) => format!("You have a new match with {name}."),
        (Language::En, NewMessage) => format!("{name} sent you a message."),
        (Language::En, MatchExpiring) => format!("Your conversation with {name} will be deleted within 7 days unless a new message is sent."),
        (Language::En, ContinuationRequested) => format!("{name} would like to continue. Would you?"),
        (Language::En, ConversationContinued) => format!("You and {name} chose to continue. You can resume your conversation."),
        (Language::En, PhotosRevealed) => format!("You and {name} can now see each other’s photos."),
        (Language::En, FirstPeriodEnding) => format!("Your first period with {name} ends in less than two hours. You can then choose whether to continue."),
        (Language::Es, NewMatch) => format!("Tienes un nuevo match con {name}."),
        (Language::Es, NewMessage) => format!("{name} te ha enviado un mensaje."),
        (Language::Es, MatchExpiring) => format!("Tu conversación con {name} se eliminará en los próximos 7 días si no se envía un nuevo mensaje."),
        (Language::Es, ContinuationRequested) => format!("{name} quiere continuar. ¿Y tú?"),
        (Language::Es, ConversationContinued) => format!("{name} y tú habéis elegido continuar. Ya podéis retomar la conversación."),
        (Language::Es, PhotosRevealed) => format!("{name} y tú ya podéis ver vuestras fotos."),
        (Language::Es, FirstPeriodEnding) => format!("Tu primer periodo con {name} termina en menos de dos horas. Después podrás decidir si quieres continuar."),
        (Language::It, NewMatch) => format!("Hai un nuovo match con {name}."),
        (Language::It, NewMessage) => format!("{name} ti ha inviato un messaggio."),
        (Language::It, MatchExpiring) => format!("La tua conversazione con {name} verrà eliminata entro 7 giorni senza un nuovo messaggio."),
        (Language::It, ContinuationRequested) => format!("{name} desidera continuare. E tu?"),
        (Language::It, ConversationContinued) => format!("Tu e {name} avete scelto di continuare. Potete riprendere la conversazione."),
        (Language::It, PhotosRevealed) => format!("Tu e {name} potete ora vedere le vostre foto."),
        (Language::It, FirstPeriodEnding) => format!("Il tuo primo periodo con {name} termina tra meno di due ore. Poi potrai scegliere se continuare."),
        _ => fallback.to_owned(),
    };
    (title, body)
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

#[cfg(test)]
mod notification_tests {
    use super::*;

    #[test]
    fn every_match_notice_uses_a_bounded_current_name_in_each_language() {
        for language in [Language::Fr, Language::En, Language::Es, Language::It] {
            for kind in [NotificationType::NewMatch, NotificationType::NewMessage,
                NotificationType::MatchExpiring, NotificationType::ContinuationRequested,
                NotificationType::ConversationContinued, NotificationType::PhotosRevealed,
                NotificationType::FirstPeriodEnding] {
                let (_, body) = personalized_notification(language, kind, Some("  Lucas\n\u{202e}  "));
                assert!(body.contains("Lucas"));
                assert!(!body.contains('\n') && !body.contains('\u{202e}'));
                let (_, fallback) = personalized_notification(language, kind, Some("\n  "));
                assert_eq!(fallback, notification(language, kind).1);
            }
        }
        assert_eq!(personalized_notification(Language::Fr, NotificationType::ContinuationRequested, Some("Lucas")).1, "Lucas souhaite continuer, et vous ?");
    }

    #[test]
    fn account_alerts_never_interpolate_an_unrelated_name() {
        for kind in [NotificationType::NewSession, NotificationType::DataRequestReplied, NotificationType::BillingPaymentFailed] {
            let (_, body) = personalized_notification(Language::Fr, kind, Some("PRIVATE_NAME"));
            assert!(!body.contains("PRIVATE_NAME"));
        }
    }
}
