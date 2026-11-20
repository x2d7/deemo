//! Единственный источник строк pp в интерфейсе.
//!
//! Жёсткое правило клиента: игра НИКОГДА не считает pp сама. Любое число pp
//! на экране — это значение из ответа сервера (`pp`, `pp_tablet`, `pp_mouse`,
//! `total_pp`), переданное в [`OnlineRecord`]/[`ReplayEntry`]/`ScoreAck` и
//! только отформатированное здесь. Нет формулы, оценки или арифметики.
//!
//! Если старый сервер поля не прислал (`None`), интерфейс показывает `—`, а не
//! ноль и не догадку. Именно поэтому все pp-строки собраны в этом модуле:
//! тест [`tests::pp_strings_live_only_here`] проверяет, что другие UI-модули
//! не собирают свои pp-подписи.
//!
//! Не путать с серверной моделью: pp считается на сервере по hit accuracy и
//! звёздам карты. Клиент её не повторяет и не «исправляет» — меньше промахов
//! не обязано давать больше pp, если точность попаданий ниже.

use crate::online::{OnlineRecord, ReplayEntry};

/// То же со знаком (как на экране результата): `+86.2` / `—`.
pub fn signed(pp: Option<f32>) -> String {
    match pp.filter(|v| v.is_finite()) {
        Some(v) => format!("{v:+.1}"),
        None => "—".to_string(),
    }
}

/// То же для `f64` из ack: `+86.2` / `—` при нечисле.
pub fn signed64(pp: f64) -> String {
    if pp.is_finite() {
        format!("{pp:+.1}")
    } else {
        "—".to_string()
    }
}

/// Без знака (`86.2`), для крупного числа в панели лучшего результата.
pub fn plain64(pp: f64) -> String {
    if pp.is_finite() {
        format!("{pp:.1}")
    } else {
        "—".to_string()
    }
}

fn num64(pp: f64) -> String {
    if pp.is_finite() {
        format!("{pp:.1}")
    } else {
        "—".to_string()
    }
}

/// Подпись для строки лидерборда: `86.2 pp` или `—`.
pub fn tag(pp: Option<f32>) -> String {
    match pp.filter(|v| v.is_finite()) {
        Some(v) => format!("{v:.1} pp"),
        None => "—".to_string(),
    }
}

/// Прибавка pp в тосте после загрузки: `+86.2 pp` (значение из ack сервера).
pub fn ack_tag(pp: f64) -> String {
    format!("{pp:+.1} pp")
}

/// Разбивка по рукам: только если сервер её реально прислал. Числа не
/// складываются и не сравниваются с общим pp — просто показываются.
pub fn hands_line(tablet: Option<f32>, mouse: Option<f32>) -> Option<String> {
    match (
        tablet.filter(|v| v.is_finite()),
        mouse.filter(|v| v.is_finite()),
    ) {
        (Some(t), Some(m)) => Some(format!("PEN {t:.1} / MOUSE {m:.1}")),
        _ => None,
    }
}

/// Строка pp в деталях плея. Только поля записи; реплей-запись используется
/// лишь как источник ника/наличия, не как альтернативный pp.
pub fn detail_line(rec: &OnlineRecord) -> String {
    let mut line = format!("PP {}", signed(rec.pp));
    if rec.hands {
        line.push_str("  ·  ");
        match hands_line(rec.pp_tablet, rec.pp_mouse) {
            Some(hands) => line.push_str(&hands),
            // поле рук отсутствует/битое — честная прочерк, не ноль
            None => line.push_str("PEN — / MOUSE —"),
        }
    }
    line
}

/// Подпись ожидания: ровно `PP PENDING`, без числа.
pub fn pending_text() -> &'static str {
    "PP PENDING"
}

/// Строка pp на экране результата: значения из ack очереди (`ScoreAck`),
/// который пришёл с сервера. `Pending` — без числа.
pub fn result_line(pp: crate::screens::ResultPp) -> Option<String> {
    match pp {
        crate::screens::ResultPp::Hidden => None,
        crate::screens::ResultPp::Pending => Some(pending_text().to_string()),
        crate::screens::ResultPp::Ready {
            pp,
            total_pp,
            pp_tablet,
            pp_mouse,
            ..
        } => {
            let total = total_pp.map(num64).unwrap_or_else(|| "—".to_string());
            let mut line = format!("PP {}  ·  TOTAL {total}", signed64(pp));
            match (pp_tablet, pp_mouse) {
                (Some(t), Some(m)) if t.is_finite() && m.is_finite() => {
                    line.push_str(&format!("  ·  PEN {t:.1} / MOUSE {m:.1}"));
                }
                _ => line.push_str("  ·  PEN — / MOUSE —"),
            }
            Some(line)
        }
    }
}

/// Проверка для экрана деталей: реплей-запись не подменяет pp записи.
pub fn replay_pp_note(rec: &OnlineRecord, replay: Option<&ReplayEntry>) -> Option<String> {
    // Реплей-запись тоже пришла с сервера; показываем её pp отдельной строкой
    // только когда у самой записи лидерборда значения нет.
    if rec.pp.is_some() {
        return None;
    }
    replay
        .and_then(|r| r.pp)
        .filter(|v| v.is_finite())
        .map(|v| format!("REPLAY PP {}", signed64(v)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(pp: Option<f32>, tablet: Option<f32>, mouse: Option<f32>, hands: bool) -> OnlineRecord {
        OnlineRecord {
            nickname: "cyrsia".into(),
            song: "ad3abiogenesis".into(),
            diff: "hard".into(),
            score: 999_192,
            accuracy: 99.92,
            accuracy_real: Some(99.19),
            pp,
            pp_tablet: tablet,
            pp_mouse: mouse,
            hands,
            max_combo: 928,
            perfect: 913,
            good: 15,
            miss: 0,
            rank: "S".into(),
            full_combo: true,
            speed: 7.7,
            timestamp: 1_794_940_377,
        }
    }

    /// Строки UI берут ровно те числа, что лежат в разобранной записи.
    #[test]
    fn displayed_values_equal_the_server_fields() {
        let r = rec(Some(86.25), Some(43.0), Some(43.25), true);
        assert_eq!(tag(r.pp), "86.2 pp");
        assert_eq!(detail_line(&r), "PP +86.2  ·  PEN 43.0 / MOUSE 43.2");
        assert_eq!(signed(r.pp), "+86.2");
    }

    /// Значения строки/деталей равны ровно полям, разобранным из JSON
    /// лидерборда (никакой локальной формулы по пути).
    #[test]
    fn row_and_detail_values_come_from_parsed_server_json() {
        let v = serde_json::json!({
            "nickname": "cyrsia", "song": "s", "diff": "hard",
            "score": 999192, "accuracy": 99.92, "accuracy_real": 99.19,
            "pp": 86.25, "pp_tablet": 43.0, "pp_mouse": 43.25,
            "hands": true, "max_combo": 928, "perfect": 913,
            "good": 15, "miss": 0, "rank": "S", "full_combo": true,
            "speed": 7.7, "timestamp": 1794940377
        });
        let rec: OnlineRecord = serde_json::from_value(v).expect("record");
        assert_eq!(tag(rec.pp), "86.2 pp");
        assert_eq!(
            detail_line(&rec),
            "PP +86.2  ·  PEN 43.0 / MOUSE 43.2"
        );
        // и никакой арифметики: руки не складываются в общий pp
        assert_eq!(rec.pp, Some(86.25));
        assert_eq!(rec.pp_tablet, Some(43.0));
    }

    /// Нет поля (старый сервер) — «—», а не 0.0 и не сумма рук.
    #[test]
    fn missing_server_pp_shows_a_dash() {
        let r = rec(None, None, None, false);
        assert_eq!(tag(r.pp), "—");
        assert_eq!(detail_line(&r), "PP —");
        assert!(!detail_line(&r).contains("0.0"));
        // даже если сервер прислал только руки, общего pp из них не выводим:
        // общий «—», руки показываются как отдельные серверные поля
        let r = rec(None, Some(43.0), Some(43.25), true);
        assert_eq!(detail_line(&r), "PP —  ·  PEN 43.0 / MOUSE 43.2");
        assert!(!detail_line(&r).contains("86"));
    }

    /// Экран результата: пока ответа нет — ровно «PP PENDING», без числа.
    #[test]
    fn result_pending_has_no_placeholder_number() {
        assert_eq!(
            result_line(crate::screens::ResultPp::Pending).as_deref(),
            Some("PP PENDING")
        );
        assert_eq!(result_line(crate::screens::ResultPp::Hidden), None);
        let ready = crate::screens::ResultPp::Ready {
            pp: 172.6,
            total_pp: Some(2760.6),
            pp_tablet: Some(86.0),
            pp_mouse: Some(86.6),
            record: true,
        };
        assert_eq!(
            result_line(ready).as_deref(),
            Some("PP +172.6  ·  TOTAL 2760.6  ·  PEN 86.0 / MOUSE 86.6")
        );
        // старый сервер: pp есть, остальных полей нет — прочерки, не нули
        let partial = crate::screens::ResultPp::Ready {
            pp: 10.0,
            total_pp: None,
            pp_tablet: None,
            pp_mouse: None,
            record: false,
        };
        let line = result_line(partial).expect("pp-only ack");
        assert!(line.contains("TOTAL —"), "{line}");
        assert!(line.contains("PEN — / MOUSE —"), "{line}");
        assert!(!line.contains("TOTAL 0.0"), "{line}");
        assert!(!line.contains("PEN 0.0"), "{line}");
    }

    /// Реплей не подменяет pp записи; его поле — только запасная подпись,
    /// когда у записи pp вообще нет.
    #[test]
    fn replay_pp_never_replaces_or_derives_the_record_pp() {
        let r = rec(Some(86.25), None, None, false);
        let replay = ReplayEntry {
            nick: "cyrsia".into(),
            score: 999_192,
            pp: Some(80.0),
            accuracy: 99.19,
            timestamp: 1,
            notes: 928,
        };
        assert_eq!(replay_pp_note(&r, Some(&replay)), None);
        let no_pp = rec(None, None, None, false);
        assert_eq!(
            replay_pp_note(&no_pp, Some(&replay)).as_deref(),
            Some("REPLAY PP +80.0")
        );
    }

    /// Grep-стиль: UI-модули не собирают pp-строки сами. Все подписи — здесь.
    #[test]
    fn pp_strings_live_only_here() {
        let sources: [(&str, &str); 3] = [
            ("screens.rs", include_str!("screens.rs")),
            ("app.rs", include_str!("app.rs")),
            ("results.rs", include_str!("results.rs")),
        ];
        for (name, src) in sources {
            for line in src.lines() {
                let t = line.trim();
                if t.starts_with("//") {
                    continue;
                }
                // строковые литералы pp в чужих модулях — нарушение правила
                for bad in ["\"PP ", "\" pp\""] {
                    assert!(
                        !line.contains(bad),
                        "{name}: pp must be formatted in pp.rs only, found {bad:?} in: {t}"
                    );
                }
            }
        }
    }
}
