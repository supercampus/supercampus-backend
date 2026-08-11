use super::CrmError;

pub const LEAD_SOURCES: [&str; 26] = [
    "Agent / Consultant",
    "Google Search",
    "Other Search / AI",
    "Google Ads",
    "Facebook / Instagram",
    "LinkedIn",
    "YouTube",
    "Quora",
    "College Portals / Aggregators",
    "Institution Website",
    "Google Business Profile",
    "Inbound Call",
    "Inbound WhatsApp",
    "Walk-In",
    "Outbound Calling",
    "WhatsApp Campaign",
    "SMS Campaign",
    "Student Referral",
    "Alumni Referral",
    "Parent Referral",
    "School / Counselor Referral",
    "Education Fair / Seminar",
    "Webinar",
    "Counselling / Admission Event",
    "Radio / Offline Media",
    "Other",
];

pub fn canonical_lead_source(value: &str) -> Result<String, CrmError> {
    let value = value.trim();
    LEAD_SOURCES
        .iter()
        .find(|source| source.eq_ignore_ascii_case(value))
        .map(|source| (*source).to_owned())
        .ok_or_else(|| {
            CrmError::Validation(format!(
                "unknown lead source: {value}. Select one of the configured lead sources"
            ))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalizes_source_case() {
        assert_eq!(canonical_lead_source("google ads").unwrap(), "Google Ads");
    }

    #[test]
    fn rejects_unconfigured_source() {
        assert!(canonical_lead_source("Legacy campaign").is_err());
    }
}
