//! WebFetch 的预批准域名（docs/specs/rust-cli/tools-more.md §4.1）：公开的规范、语言与框架文档站。命中时不询问，
//! 但 deny / ask / allow 的 `domain:` 规则先于它生效。

use url::Url;

/// 预批准列表：没有 `/` 的项按主机名匹配，有 `/` 的项按主机名加路径前缀匹配。
const PREAPPROVED: &[&str] = &[
    "modelcontextprotocol.io",
    "agentskills.io",
    "docs.python.org",
    "en.cppreference.com",
    "docs.oracle.com",
    "learn.microsoft.com",
    "developer.mozilla.org",
    "go.dev/doc",
    "go.dev/ref",
    "www.php.net",
    "docs.swift.org",
    "kotlinlang.org",
    "ruby-doc.org",
    "doc.rust-lang.org",
    "www.typescriptlang.org",
    "react.dev",
    "angular.io",
    "vuejs.org",
    "nextjs.org",
    "expressjs.com",
    "nodejs.org",
    "bun.sh",
    "jquery.com",
    "getbootstrap.com",
    "tailwindcss.com",
    "d3js.org",
    "threejs.org",
    "redux.js.org",
    "webpack.js.org",
    "jestjs.io",
    "reactrouter.com",
    "docs.djangoproject.com",
    "flask.palletsprojects.com",
    "fastapi.tiangolo.com",
    "pandas.pydata.org",
    "numpy.org",
    "www.tensorflow.org",
    "pytorch.org",
    "scikit-learn.org",
    "matplotlib.org",
    "requests.readthedocs.io",
    "jupyter.org",
    "laravel.com",
    "symfony.com",
    "wordpress.org/documentation",
    "docs.spring.io",
    "hibernate.org",
    "tomcat.apache.org",
    "gradle.org",
    "maven.apache.org",
    "asp.net",
    "dotnet.microsoft.com",
    "blazor.net",
    "reactnative.dev",
    "docs.flutter.dev",
    "developer.apple.com",
    "developer.android.com",
    "keras.io",
    "spark.apache.org",
    "huggingface.co/docs",
    "www.kaggle.com/docs",
    "www.mongodb.com",
    "redis.io",
    "www.postgresql.org",
    "dev.mysql.com",
    "www.sqlite.org",
    "graphql.org",
    "prisma.io",
    "docs.getdbt.com",
    "docs.aws.amazon.com",
    "cloud.google.com",
    "kubernetes.io",
    "www.docker.com",
    "www.terraform.io",
    "www.ansible.com",
    "vercel.com/docs",
    "docs.stripe.com",
    "docs.netlify.com",
    "devcenter.heroku.com",
    "dev.wix.com/docs",
    "cypress.io",
    "selenium.dev",
    "docs.unity.com",
    "docs.unrealengine.com",
    "git-scm.com",
    "nginx.org",
    "httpd.apache.org",
];

/// 路径在带路径的列表项之下（等于前缀或以 `前缀/` 开头）。编码过的 `/`、`\`、`.` 可能绕过前缀，
/// 这类路径一律不算（先拒绝它们）。
fn under(path: &str, entry_path: &str) -> bool {
    let lowered = path.to_ascii_lowercase();
    if ["%2f", "%5c", "%2e", "%252f", "%255c", "%252e"]
        .iter()
        .any(|encoded| lowered.contains(encoded))
    {
        return false;
    }
    let prefix = format!("/{entry_path}");
    path == prefix || path.starts_with(&format!("{prefix}/"))
}

/// 主机名与路径是否预批准（WebFetch 判断同站重定向时也用）。
pub fn host_path(host: &str, path: &str) -> bool {
    PREAPPROVED.iter().any(|entry| match entry.split_once('/') {
        None => *entry == host,
        Some((entry_host, entry_path)) => entry_host == host && under(path, entry_path),
    })
}

/// URL 的主机与路径是否在预批准列表里。
pub fn preapproved(url: &str) -> bool {
    Url::parse(url)
        .ok()
        .and_then(|url| Some((url.host_str()?.to_owned(), url.path().to_owned())))
        .is_some_and(|(host, path)| host_path(&host, &path))
}

#[cfg(test)]
#[path = "web_preapproved_tests.rs"]
mod tests;
