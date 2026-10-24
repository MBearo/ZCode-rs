//! `Pdf`：pdfinfo 与 pdftoppm 的调用。Unix 上用临时目录里的假程序（shell 脚本，只记下参数与 stdin、写出约定的输出）
//! 核对命令行、stdin、页面的顺序与输出目录的清理；找不到程序的情形三平台都测。

use std::path::PathBuf;

use rust_cli_env_local::{LocalEnvironment, ShellConfig};
use rust_cli_kernel_api::env::EnvPath;
use rust_cli_kernel_api::env_pdf::{PdfPageCount, PdfRender, RenderPdfPages};

use crate::contract;
use crate::support::{Kind, Machine, machine, workspace};

const PDF: &[u8] = b"%PDF-1.4\n1 0 obj<</Type/Catalog>>endobj\n%%EOF\n";

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    #[cfg_attr(not(unix), expect(dead_code, reason = "假程序只在 Unix 上写"))]
    bin: PathBuf,
    scratch: PathBuf,
    machine: Machine,
}

async fn fixture(kind: Kind) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let (root, bin, scratch) = (base.join("work"), base.join("bin"), base.join("scratch"));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(root.join("doc.pdf"), PDF).unwrap();
    // PATH 只有假程序的目录：机器上装没装 poppler 都不影响结果。
    let path = bin.to_string_lossy().into_owned();
    let local = LocalEnvironment::new(Some(base.clone())).with_shell(ShellConfig {
        scratch: scratch.clone(),
        ledger: base.join("ledger"),
        bash: None,
        bwrap: None,
        embedded: None,
        env: vec![("PATH".to_owned(), path)].into(),
    });
    Fixture {
        _dir: dir,
        root,
        bin,
        scratch,
        machine: machine(kind, local).await,
    }
}

impl Fixture {
    fn doc(&self) -> EnvPath {
        EnvPath::new(self.root.join("doc.pdf").to_string_lossy())
    }

    #[cfg(unix)]
    fn program(&self, name: &str, script: &str) {
        use std::os::unix::fs::PermissionsExt;
        let path = self.bin.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{script}")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(unix)]
    fn read(&self, name: &str) -> Vec<u8> {
        std::fs::read(self.bin.join(name)).unwrap()
    }

    async fn count(&self) -> Option<u64> {
        let request = PdfPageCount { path: self.doc() };
        self.machine
            .pdf()
            .pdf_page_count(&workspace(&self.root), request)
            .await
            .unwrap()
    }

    async fn render(&self, first_page: u64, last_page: Option<u64>) -> PdfRender {
        let request = RenderPdfPages {
            path: self.doc(),
            first_page,
            last_page,
        };
        self.machine
            .pdf()
            .render_pdf_pages(&workspace(&self.root), request)
            .await
            .unwrap()
    }

    fn no_output_left(&self) -> bool {
        std::fs::read_dir(self.scratch.join("pdf"))
            .map(|entries| entries.count() == 0)
            .unwrap_or(true)
    }
}

async fn without_poppler_there_is_no_count_and_no_renderer(kind: Kind) {
    let fixture = fixture(kind).await;
    assert_eq!(fixture.count().await, None);
    assert_eq!(fixture.render(1, Some(2)).await, PdfRender::Unavailable);
    assert!(fixture.no_output_left());
}

#[cfg(unix)]
async fn pdfinfo_gets_the_file_on_stdin(kind: Kind) {
    let fixture = fixture(kind).await;
    let bin = fixture.bin.to_string_lossy().into_owned();
    fixture.program(
        "pdfinfo",
        &format!(
            "echo \"$@\" > {bin}/info.args\n/bin/cat > {bin}/info.stdin\nprintf 'Title:          x\\nPages:          12\\nEncrypted:      no\\n'\n"
        ),
    );
    assert_eq!(fixture.count().await, Some(12));
    assert_eq!(fixture.read("info.args"), b"-\n");
    assert_eq!(fixture.read("info.stdin"), PDF);
    fixture.program(
        "pdfinfo",
        "/bin/cat > /dev/null\necho 'Syntax Error' >&2\nexit 1\n",
    );
    assert_eq!(fixture.count().await, None);
    fixture.program("pdfinfo", "/bin/cat > /dev/null\necho 'Title: x'\n");
    assert_eq!(fixture.count().await, None);
}

#[cfg(unix)]
async fn pdftoppm_renders_the_range_with_the_expected_arguments(kind: Kind) {
    let fixture = fixture(kind).await;
    let bin = fixture.bin.to_string_lossy().into_owned();
    fixture.program(
        "pdftoppm",
        &format!(
            "if [ \"$1\" = -v ]; then echo 'pdftoppm version 24.02.0' >&2; exit 0; fi\n\
             echo \"$@\" > {bin}/render.args\n/bin/cat > {bin}/render.stdin\n\
             for last; do :; done\n\
             printf 'ten' > \"$last-10.jpg\"\nprintf 'nine' > \"$last-09.jpg\"\nprintf 'eight' > \"$last-08.jpg\"\n\
             printf 'x' > \"$last-08.ppm\"\n"
        ),
    );
    let rendered = fixture.render(8, Some(10)).await;
    assert_eq!(
        rendered,
        PdfRender::Rendered {
            pages: vec![b"eight".to_vec(), b"nine".to_vec(), b"ten".to_vec()]
        }
    );
    let args = String::from_utf8(fixture.read("render.args")).unwrap();
    let prefix = fixture.scratch.join("pdf");
    let expected = format!("-jpeg -r 100 -f 8 -l 10 - {}/", prefix.to_string_lossy());
    assert!(args.starts_with(&expected), "{args}");
    assert!(args.trim_end().ends_with("/page"), "{args}");
    assert_eq!(fixture.read("render.stdin"), PDF);
    assert!(fixture.no_output_left());
    fixture.render(3, None).await;
    let args = String::from_utf8(fixture.read("render.args")).unwrap();
    assert!(args.starts_with("-jpeg -r 100 -f 3 - "), "{args}");
}

#[cfg(unix)]
async fn pdftoppm_failures_keep_the_exit_code_and_stderr(kind: Kind) {
    let fixture = fixture(kind).await;
    fixture.program(
        "pdftoppm",
        "if [ \"$1\" = -v ]; then echo 'pdftoppm version 0.86' >&2; exit 99; fi\n\
         /bin/cat > /dev/null\n\
         echo 'Wrong page range given: the first page (2) can not be after the last page (1).' >&2\nexit 99\n",
    );
    assert_eq!(
        fixture.render(2, Some(3)).await,
        PdfRender::Failed {
            exit_code: Some(99),
            stderr:
                "Wrong page range given: the first page (2) can not be after the last page (1).\n"
                    .to_owned(),
        }
    );
    assert!(fixture.no_output_left());
}

#[cfg(unix)]
async fn an_unusable_pdftoppm_is_unavailable(kind: Kind) {
    let fixture = fixture(kind).await;
    fixture.program("pdftoppm", "echo 'not found' >&2\nexit 127\n");
    assert_eq!(fixture.render(1, Some(1)).await, PdfRender::Unavailable);
    fixture.program("pdftoppm", "exit 3\n");
    assert_eq!(fixture.render(1, Some(1)).await, PdfRender::Unavailable);
}

contract!(without_poppler_there_is_no_count_and_no_renderer);
#[cfg(unix)]
contract!(
    pdfinfo_gets_the_file_on_stdin,
    pdftoppm_renders_the_range_with_the_expected_arguments,
    pdftoppm_failures_keep_the_exit_code_and_stderr,
    an_unusable_pdftoppm_is_unavailable,
);
