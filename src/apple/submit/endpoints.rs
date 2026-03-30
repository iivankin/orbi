const DEFAULT_CONTENT_DELIVERY_BASE_URL: &str = "https://contentdelivery.itunes.apple.com";
const DEFAULT_NOTARY_BASE_URL: &str = "https://appstoreconnect.apple.com";
const IRIS_PATH: &str = "/MZContentDeliveryService/iris";

pub fn label_service_url() -> String {
    label_service_url_for("MZITunesSoftwareService")
}

pub fn label_service_url_for(service_class: &str) -> String {
    if service_class == "MZITunesSoftwareService" {
        return std::env::var("ORBIT_LABEL_SERVICE_URL").unwrap_or_else(|_| {
            format!(
                "{}/WebObjects/MZLabelService.woa/json/{}",
                content_delivery_base_url(),
                service_class
            )
        });
    }

    format!(
        "{}/WebObjects/MZLabelService.woa/json/{}",
        content_delivery_base_url(),
        service_class
    )
}

pub fn iris_base_url() -> String {
    std::env::var("ORBIT_IRIS_BASE_URL")
        .unwrap_or_else(|_| format!("{}{}", content_delivery_base_url(), IRIS_PATH))
}

pub fn uses_mock_content_delivery() -> bool {
    std::env::var_os("ORBIT_CONTENT_DELIVERY_BASE_URL").is_some()
}

pub fn notary_base_url() -> String {
    std::env::var("ORBIT_NOTARY_BASE_URL").unwrap_or_else(|_| DEFAULT_NOTARY_BASE_URL.to_owned())
}

pub fn notary_submissions_url() -> String {
    format!("{}/notary/v2/submissions", notary_base_url())
}

pub fn notary_submission_url(submission_id: &str) -> String {
    format!(
        "{}/notary/v2/submissions/{submission_id}",
        notary_base_url()
    )
}

pub fn notary_submission_logs_url(submission_id: &str) -> String {
    format!(
        "{}/notary/v2/submissions/{submission_id}/logs",
        notary_base_url()
    )
}

pub fn notary_upload_url(bucket: &str, object: &str) -> String {
    if let Ok(base_url) = std::env::var("ORBIT_NOTARY_UPLOAD_BASE_URL") {
        return format!(
            "{}/{}",
            base_url.trim_end_matches('/'),
            object.trim_start_matches('/')
        );
    }

    format!(
        "https://{}.s3-accelerate.amazonaws.com/{}",
        bucket,
        object.trim_start_matches('/')
    )
}

fn content_delivery_base_url() -> String {
    std::env::var("ORBIT_CONTENT_DELIVERY_BASE_URL")
        .unwrap_or_else(|_| DEFAULT_CONTENT_DELIVERY_BASE_URL.to_owned())
}
