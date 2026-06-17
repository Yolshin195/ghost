// build.rs — генерирует self-signed TLS сертификат при сборке проекта.
// Запускается автоматически: cargo build / cargo run
// Файлы создаются рядом с Cargo.toml (в корне проекта).
// Пересоздаются только если отсутствуют — уже существующие не трогаются.

fn main() {
    // Сообщаем cargo: перезапускать build.rs только если сами файлы исчезли.
    // Это предотвращает лишние пересборки при каждом `cargo build`.
    println!("cargo:rerun-if-changed=cert.pem");
    println!("cargo:rerun-if-changed=key.pem");

    let cert_path = std::path::Path::new("cert.pem");
    let key_path = std::path::Path::new("key.pem");

    if cert_path.exists() && key_path.exists() {
        println!("cargo:warning=TLS: cert.pem и key.pem уже существуют, пропускаем генерацию.");
        return;
    }

    println!("cargo:warning=TLS: генерируем самоподписанный сертификат...");

    // rcgen доступен как build-dependency, не попадает в финальный бинарник
    use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, SanType};

    let mut params = CertificateParams::default();

    // Subject: CN=localhost
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, "localhost");
    params.distinguished_name = dn;

    // Subject Alternative Names — браузеры проверяют именно SAN,
    // поэтому добавляем и DNS-имена и IP-адреса для локальной сети
    params.subject_alt_names = vec![
        SanType::DnsName("localhost".try_into().expect("valid dns name")),
        // IP локальной сети — при необходимости добавить свой адрес
        SanType::IpAddress(std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1))),
        SanType::IpAddress(std::net::IpAddr::V4(std::net::Ipv4Addr::new(0, 0, 0, 0))),
        SanType::IpAddress(std::net::IpAddr::V4(std::net::Ipv4Addr::new(
            172, 20, 10, 2,
        ))),
    ];

    // Срок действия: 10 лет (для локальной разработки удобнее чем 365 дней)
    params.not_before = time::OffsetDateTime::now_utc();
    params.not_after = time::OffsetDateTime::now_utc() + time::Duration::days(3650);

    let key_pair = KeyPair::generate().expect("keygen failed");
    let cert = params
        .self_signed(&key_pair)
        .expect("cert generation failed");

    std::fs::write(cert_path, cert.pem()).expect("не удалось записать cert.pem");
    std::fs::write(key_path, key_pair.serialize_pem()).expect("не удалось записать key.pem");

    println!("cargo:warning=TLS: cert.pem и key.pem успешно созданы.");
}
