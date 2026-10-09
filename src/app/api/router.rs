use std::sync::Arc;

use axum::Router;

use crate::administration::http::{AdministrationHttpState, routes as administration_routes};
use crate::administration::metrics::{AdminMetricsService, PgAdminMetricsRepository};
use crate::administration::pg::PgAdministrationStore;
use crate::administration::photos::http::{AdminPhotoHttpState, routes as admin_photo_routes};
use crate::administration::photos::{AdminPhotoService, PgAdminPhotoRepository};
use crate::administration::service::AdministrationService;
use crate::billing::http::{
    BillingHttpState, StripeWebhookHttpState, routes as billing_routes, stripe_webhook_routes,
};
use crate::billing::pg::PgBillingRepository;
use crate::billing::reconcile::{
    BillingReconciliationHttpState, BillingReconciliationService, PgBillingReconciliationStore,
    routes as billing_reconciliation_routes,
};
use crate::billing::service::BillingService;
use crate::billing::stripe::StripeClient;
use crate::billing::webhook::{PgStripeWebhookStore, StripeWebhookService};
use crate::catalog::http::{CatalogHttpState, routes as catalog_routes};
use crate::catalog::pg::PgCatalogRepository;
use crate::catalog::service::CatalogService;
use crate::config::{AppConfig, RateLimitStore, SmsProvider};
use crate::discovery::cursor::FeedCursorCodec;
use crate::discovery::http::{DiscoveryHttpState, routes as discovery_routes};
use crate::discovery::pg::{PgDiscoveryRepository, PgSwipeStore};
use crate::discovery::service::DiscoveryService;
use crate::http::health::{DependencyProbe, Readiness};
use crate::http::rate_limit::RateLimiter;
use crate::http::{HttpState, build_router, health_routes};
use crate::identity::admin::http::{AdminAuthHttpState, routes as admin_auth_routes};
use crate::identity::admin::pg::AdminAuthRepository;
use crate::identity::admin::service::AdminAuthService;
use crate::identity::mobile::account::{MobileAccountRepository, MobileLoginService};
use crate::identity::mobile::http::{
    MobileAuthState, OtpHttpState, SweegoHttpState, otp_routes, routes as mobile_auth_routes,
    sweego_routes,
};
use crate::identity::mobile::otp::{OtpRepository, OtpService};
use crate::identity::mobile::pg::MobileSessionRepository;
use crate::identity::mobile::service::MobileAuthService;
use crate::identity::mobile::sweego::{SmsDelivery, SweegoSmsService, SweegoWebhookService};
use crate::identity::mobile::tokens::TokenService;
use crate::matches::http::{MatchHttpState, routes as match_routes};
use crate::matches::pg::{PgMatchMessageRepository, PgMatchRepository};
use crate::matches::service::{MatchEventPublisher, MatchService};
use crate::media::codec::PhotoCodec;
use crate::media::http::{PhotoHttpState, routes as photo_routes};
use crate::media::pg::PgPhotoRepository;
use crate::media::service::{PhotoProcessor, PhotoService};
use crate::media::storage::PhotoObjectStorage;
use crate::media::store::PhotoStore;
use crate::moderation::http::{ModerationHttpState, routes as moderation_routes};
use crate::moderation::pg::PgModerationRepository;
use crate::moderation::photo::{HttpPhotoModerator, PhotoModerator};
use crate::moderation::service::{ModerationPhotoUrlProvider, ModerationService};
use crate::notifications::delivery::MobileDeliveryService;
use crate::notifications::devices::DeviceService;
use crate::notifications::http::{NotificationHttpState, routes as notification_routes};
use crate::notifications::pg::PgNotificationRepository;
use crate::notifications::sse::{SessionActivity, SseHttpState, routes as sse_routes};
use crate::operations::status::{DependencyConfiguration, OperationalStatusView};
use crate::outbox::admin::{OutboxAdminService, PgOutboxAdminRepository};
use crate::outbox::http::{OutboxAdminHttpState, routes as outbox_admin_routes};
use crate::outbox::pg::PgOutboxRepository;
use crate::privacy::erasure::AccountDeletionService;
use crate::privacy::erasure::http::{AccountDeletionHttpState, routes as account_deletion_routes};
use crate::privacy::erasure::pg::PgErasureRepository;
use crate::privacy::export::DataExportService;
use crate::privacy::export::http::{DataExportHttpState, routes as data_export_routes};
use crate::privacy::export::pg::PgDataExportStore;
use crate::privacy::http::{PrivacyHttpState, routes as privacy_routes};
use crate::privacy::pg::PgPrivacyStore;
use crate::privacy::rights::DataRightsService;
use crate::privacy::rights::http::{
    DataRightsHttpState, admin_routes as admin_data_rights_routes,
    mobile_routes as mobile_data_rights_routes,
};
use crate::privacy::rights::pg::PgDataRightsStore;
use crate::privacy::service::{PrivacyEventPublisher, PrivacyService};
use crate::profiles::http::{ProfileHttpState, routes as profile_routes};
use crate::profiles::pg::PgProfileRepository;
use crate::profiles::service::{ProfilePhotoUrlProvider, ProfileService};
use crate::reports::http::{
    ReportHttpState, admin_routes as admin_report_routes, mobile_routes as mobile_report_routes,
};
use crate::reports::pg::PgReportStore;
use crate::reports::service::ReportService;
use crate::shared::clock::{Clock, SystemClock};

use super::resources::ApiResources;

impl ApiResources {
    pub(super) fn router(&self, config: &AppConfig) -> Result<Router, &'static str> {
        let limiter = match config.rate_limit.store {
            RateLimitStore::Memory => RateLimiter::memory(&config.phone.hash_key),
            RateLimitStore::Redis => RateLimiter::redis(&config.phone.hash_key, self.redis.clone()),
        };
        let clock: Arc<dyn Clock> = Arc::new(SystemClock);
        let outbox = PgOutboxRepository::new(self.database.clone());

        let sessions = Arc::new(MobileSessionRepository::new(self.database.clone()));
        let token_service = TokenService::new(config.jwt.clone());
        let mobile_service = MobileAuthService::new(
            token_service,
            sessions.clone(),
            config.legal.terms_version.clone(),
            config.legal.privacy_version.clone(),
        );
        let mobile_auth = MobileAuthState::new(
            mobile_service.clone(),
            limiter.clone(),
            config.rate_limit.refresh.clone(),
        );

        let otp_repository = Arc::new(OtpRepository::new(self.database.clone()));
        let sms: Arc<dyn SmsDelivery> = Arc::new(
            SweegoSmsService::new(config.sms.clone())
                .map_err(|_| "sms_client_invalid")?
                .with_metrics(Arc::clone(&self.metrics)),
        );
        let otp = OtpService::new(
            otp_repository.clone(),
            sms,
            config.phone.hash_key.clone(),
            config.sms.region.clone(),
            config.sms.timeout,
            config.sms.otp_ttl,
        );
        let login = MobileLoginService::new(
            otp.clone(),
            Arc::new(MobileAccountRepository::new(self.database.clone())),
            mobile_service,
            config.phone.encryption_key.clone(),
        );
        let sweego_webhook = SweegoWebhookService::new(
            &config.sms,
            otp_repository.clone(),
            Arc::clone(&self.callbacks),
        );

        let admin_service = AdminAuthService::new(
            Arc::new(AdminAuthRepository::new(self.database.clone())),
            config.admin_auth.clone(),
        )
        .map_err(|_| "admin_webauthn_configuration_invalid")?;
        let admin_auth = AdminAuthHttpState::new(
            admin_service,
            limiter.clone(),
            config.rate_limit.admin_auth.clone(),
            config.admin_auth.clone(),
        );

        let storage: Arc<dyn PhotoObjectStorage> = self.storage.clone();
        let photo_repository = Arc::new(PgPhotoRepository::new(
            self.database.clone(),
            outbox.clone(),
        ));
        let photo_store: Arc<dyn PhotoStore> = photo_repository;
        let processor: Arc<dyn PhotoProcessor> = Arc::new(PhotoCodec::for_runtime());
        let photo_moderator: Arc<dyn PhotoModerator> = Arc::new(
            HttpPhotoModerator::new(&config.photo_moderation)
                .map_err(|_| "photo_moderation_invalid_configuration")?,
        );
        let photo_service = PhotoService::new(
            photo_store,
            processor,
            Arc::clone(&storage),
            photo_moderator,
            self.activity.clone(),
            Arc::clone(&clock),
        );
        let photo_urls: Arc<dyn ProfilePhotoUrlProvider> = Arc::new(photo_service.clone());

        let profile = ProfileService::new(
            Arc::new(PgProfileRepository::new(self.database.clone())),
            config.legal.clone(),
            Arc::clone(&clock),
            Arc::clone(&photo_urls),
        );
        let catalog =
            CatalogService::new(Arc::new(PgCatalogRepository::new(self.database.clone())));

        let delivery = Arc::new(MobileDeliveryService::new(self.realtime.clone()));
        let match_events: Arc<dyn MatchEventPublisher> = delivery.clone();
        let matches = MatchService::new(
            Arc::new(PgMatchRepository::new(self.database.clone())),
            Arc::new(PgMatchMessageRepository::new(self.database.clone())),
            Arc::clone(&photo_urls),
            match_events,
            Arc::clone(&clock),
        );
        let discovery = DiscoveryService::new(
            Arc::new(PgDiscoveryRepository::new(self.database.clone())),
            Arc::new(PgSwipeStore::new(
                self.database.clone(),
                self.activity.clone(),
            )),
            Arc::new(matches.clone()),
            config.legal.clone(),
            FeedCursorCodec::new(&config.jwt.secret, Arc::clone(&clock))
                .map_err(|_| "discovery_invalid_cursor_configuration")?,
        );

        let stripe = Arc::new(
            StripeClient::new(config.billing.clone())
                .map_err(|_| "stripe_invalid_configuration")?
                .with_metrics(Arc::clone(&self.metrics)),
        );
        let billing = Arc::new(BillingService::new(
            Arc::new(PgBillingRepository::new(self.database.clone())),
            stripe.clone(),
            config.billing.clone(),
            self.activity.clone(),
            Arc::clone(&clock),
        ));
        let reconciliation = BillingReconciliationService::new(
            Arc::new(PgBillingReconciliationStore::new(self.database.clone())),
            stripe.clone(),
            delivery.clone(),
            Arc::new(self.activity.clone()),
            Arc::clone(&clock),
            config.billing.clone(),
        );
        let stripe_webhook = StripeWebhookService::new(
            Arc::new(PgStripeWebhookStore::new(self.database.clone())),
            stripe.clone(),
            delivery.clone(),
            Arc::clone(&clock),
            config.billing.clone(),
        );

        let privacy_events: Arc<dyn PrivacyEventPublisher> = delivery.clone();
        let privacy = PrivacyService::new(
            Arc::new(PgPrivacyStore::new(self.database.clone())),
            privacy_events,
        );
        let reports = ReportService::new(Arc::new(PgReportStore::new(self.database.clone())));
        let rights_store = Arc::new(PgDataRightsStore::new(self.database.clone()));
        let rights = DataRightsService::new(rights_store.clone());
        let export = DataExportService::new(
            Arc::new(PgDataExportStore::new(self.database.clone())),
            rights_store,
            Arc::clone(&photo_urls),
            config.workloads.data_export_page_size,
            config.workloads.data_export_max_bytes,
            config.workloads.data_export_max_concurrency,
        );
        let deletion = AccountDeletionService::new(
            Arc::new(PgErasureRepository::new(self.database.clone())),
            config.account_deletion_token_ttl,
            Arc::clone(&clock),
        );

        let moderation_photos: Arc<dyn ModerationPhotoUrlProvider> =
            Arc::new(photo_service.clone());
        let moderation = ModerationService::new(
            Arc::new(PgModerationRepository::new(
                self.database.clone(),
                outbox.clone(),
            )),
            moderation_photos,
        );

        let operations = Arc::new(OperationalStatusView::new(
            Arc::clone(&self.metrics),
            self.persistent_status.clone(),
            DependencyConfiguration {
                redis: self.redis.enabled(),
                sweego: config.sms.provider == SmsProvider::Sweego,
                stripe: config.billing.provider == crate::config::BillingProvider::Stripe,
            },
            config.billing.reconciliation_interval,
        ));
        let admin_metrics = AdminMetricsService::new(
            Arc::new(PgAdminMetricsRepository::new(
                self.database.clone(),
                config.legal.terms_version.clone(),
                config.legal.privacy_version.clone(),
            )),
            operations,
        );
        let administration = AdministrationService::new(
            Arc::new(PgAdministrationStore::new(self.database.clone())),
            Arc::clone(&photo_urls),
            config.legal.terms_version.clone(),
            config.legal.privacy_version.clone(),
        );

        let routes = health_routes()
            .merge(mobile_auth_routes(mobile_auth.clone()))
            .merge(otp_routes(OtpHttpState::new(
                otp,
                login,
                limiter.clone(),
                config.rate_limit.otp.clone(),
            )))
            .merge(sweego_routes(SweegoHttpState::new(
                sweego_webhook,
                limiter.clone(),
                config.rate_limit.sms_webhook.clone(),
            )))
            .merge(admin_auth_routes(admin_auth.clone()))
            .merge(profile_routes(
                ProfileHttpState::new(profile),
                mobile_auth.clone(),
            ))
            .merge(catalog_routes(
                CatalogHttpState::new(catalog),
                mobile_auth.clone(),
                admin_auth.clone(),
            ))
            .merge(photo_routes(
                PhotoHttpState::new(
                    photo_service,
                    limiter.clone(),
                    config.rate_limit.photo.clone(),
                ),
                mobile_auth.clone(),
            ))
            .merge(match_routes(
                MatchHttpState::new(matches, limiter.clone(), config.rate_limit.message.clone()),
                mobile_auth.clone(),
            ))
            .merge(discovery_routes(
                DiscoveryHttpState::new(
                    discovery,
                    limiter.clone(),
                    config.rate_limit.feed.clone(),
                    config.rate_limit.swipe.clone(),
                ),
                mobile_auth.clone(),
            ))
            .merge(notification_routes(
                NotificationHttpState::new(DeviceService::new(Arc::new(
                    PgNotificationRepository::new(self.database.clone()),
                ))),
                mobile_auth.clone(),
            ))
            .merge(sse_routes(
                SseHttpState::new(
                    self.realtime.clone(),
                    sessions.clone() as Arc<dyn SessionActivity>,
                ),
                mobile_auth.clone(),
            ))
            .merge(billing_routes(
                BillingHttpState::new(billing, limiter.clone(), config.rate_limit.billing.clone()),
                mobile_auth.clone(),
            ))
            .merge(stripe_webhook_routes(StripeWebhookHttpState::new(
                stripe_webhook,
                limiter.clone(),
                config.rate_limit.billing_webhook.clone(),
            )))
            .merge(privacy_routes(
                PrivacyHttpState::new(privacy),
                mobile_auth.clone(),
            ))
            .merge(mobile_report_routes(
                ReportHttpState::new(
                    reports.clone(),
                    limiter.clone(),
                    config.rate_limit.report.clone(),
                ),
                mobile_auth.clone(),
            ))
            .merge(admin_report_routes(
                ReportHttpState::new(reports, limiter.clone(), config.rate_limit.report.clone()),
                admin_auth.clone(),
            ))
            .merge(mobile_data_rights_routes(
                DataRightsHttpState::new(rights.clone()),
                mobile_auth.clone(),
            ))
            .merge(admin_data_rights_routes(
                DataRightsHttpState::new(rights),
                admin_auth.clone(),
            ))
            .merge(data_export_routes(
                DataExportHttpState::new(
                    export,
                    limiter.clone(),
                    config.rate_limit.data_export.clone(),
                ),
                mobile_auth.clone(),
            ))
            .merge(account_deletion_routes(
                AccountDeletionHttpState::new(deletion),
                mobile_auth.clone(),
            ))
            .merge(moderation_routes(
                ModerationHttpState::new(moderation),
                admin_auth.clone(),
            ))
            .merge(administration_routes(
                AdministrationHttpState::new(administration, admin_metrics),
                admin_auth.clone(),
            ))
            .merge(admin_photo_routes(
                AdminPhotoHttpState::new(AdminPhotoService::new(
                    Arc::new(PgAdminPhotoRepository::new(
                        self.database.clone(),
                        outbox.clone(),
                    )),
                    Arc::clone(&clock),
                )),
                admin_auth.clone(),
            ))
            .merge(outbox_admin_routes(
                OutboxAdminHttpState::new(OutboxAdminService::new(Arc::new(
                    PgOutboxAdminRepository::new(self.database.clone()),
                ))),
                admin_auth.clone(),
            ))
            .merge(billing_reconciliation_routes(
                BillingReconciliationHttpState::new(Arc::new(reconciliation)),
                admin_auth,
            ));

        let postgres_probe: Arc<dyn DependencyProbe> = Arc::new(self.database.clone());
        let redis_probe: Arc<dyn DependencyProbe> = Arc::new(self.redis.clone());
        let storage_probe: Arc<dyn DependencyProbe> = self.storage.clone();
        let readiness = Readiness::new(postgres_probe, redis_probe, storage_probe);
        let http_state = HttpState::with_observer(
            readiness,
            config.environment,
            &config.trust_proxy,
            &config.cors_origins,
            limiter,
            config.rate_limit.global.clone(),
            Arc::new(crate::http::lifecycle::CompositeHttpObserver::new(vec![
                self.metrics.clone(),
                Arc::new(crate::http::lifecycle::SafeHttpObserver),
            ])),
        )
        .map_err(|_| "http_configuration_invalid")?;
        Ok(build_router(routes, http_state))
    }
}
