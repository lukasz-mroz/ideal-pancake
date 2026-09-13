use rusqlite::{Connection, named_params};
use rusqlite_from_row::FromRow;
use crate::entity::setting::Setting;

pub fn insert_or_update_setting(db: &Connection, setting: Setting) -> Result<(), rusqlite::Error> {
    // Secrets (API keys) are wrapped with DPAPI before they touch the file.
    let stored_value = if crate::configuration::secret::is_secret_key(&setting.setting_key) {
        crate::configuration::secret::protect(&setting.setting_value)
    } else {
        setting.setting_value.clone()
    };

    let mut insert_statement = db.prepare("
    INSERT INTO settings (setting_key, setting_value)
    VALUES (@setting_key, @setting_value)
    ON CONFLICT(setting_key) DO NOTHING;")?;

    insert_statement.execute(named_params! {
        "@setting_key": setting.setting_key,
        "@setting_value": stored_value,
    })?;

    let mut update_statement = db.prepare("
    UPDATE settings
    SET setting_value = @setting_value
    WHERE setting_key = @setting_key;")?;

    update_statement.execute(named_params! {
        "@setting_value": stored_value,
        "@setting_key": setting.setting_key,
    })?;
    Ok(())
}

/// Read one setting. A missing row yields an empty value, which callers treat
/// as "not configured".
///
/// Anything other than a missing row is logged: a failing database used to be
/// indistinguishable from an unset option, which is how a feature can look
/// enabled in the UI while never running.
pub fn get_setting(db: &Connection, setting_key: &str) -> Result<Setting, rusqlite::Error> {
    let row = db.query_row(
        "SELECT * FROM settings WHERE setting_key = @setting_key LIMIT 1",
        named_params! {
            "@setting_key": setting_key,
        },
        Setting::try_from_row,
    );

    match row {
        Ok(mut setting) => {
            if crate::configuration::secret::is_secret_key(&setting.setting_key) {
                setting.setting_value =
                    crate::configuration::secret::reveal(&setting.setting_value);
            }
            Ok(setting)
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(Setting {
            setting_key: setting_key.to_string(),
            setting_value: String::new(),
        }),
        Err(err) => {
            log::warn!("Could not read setting {}: {}", setting_key, err);
            Ok(Setting {
                setting_key: setting_key.to_string(),
                setting_value: String::new(),
            })
        }
    }
}

pub fn get_settings(db: &Connection) -> Result<Vec<Setting>, rusqlite::Error> {
    let mut statement = db.prepare("SELECT * FROM settings")?;
    let mut rows = statement.query([])?;
    let mut settings: Vec<Setting> = Vec::new();
    while let Some(row) = rows.next()? {
        let key: String = row.get("setting_key")?;
        let mut value: String = row.get("setting_value")?;
        if crate::configuration::secret::is_secret_key(&key) {
            value = crate::configuration::secret::reveal(&value);
        }
        settings.push(Setting {
            setting_key: key,
            setting_value: value,
        });
    }

    Ok(settings)
}

