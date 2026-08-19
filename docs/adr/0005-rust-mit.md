# 0005: Rust Implementation, MIT License

We implement git-forge in Rust using libgit2 (the `git2` crate) for repository access and shell out to the `git` binary for history-level operations (`merge`, `fast-import`, `push`) to avoid reimplementing git semantics. The project is MIT licensed.

Why: the hardest part is COB-style concurrency/state folding, and the mature reference implementation (Radicle COB) is Rust; `git2` handles complex repository/ref/protocol cases more completely than `go-git`. MIT matches the ecosystem of Gitea/Gogs/OneDev and minimizes obligations for a personal local tool.

Consequences: Rust ownership/borrowing adds implementation friction relative to Go, but correctness around refs and concurrency is the dominant risk for this design.