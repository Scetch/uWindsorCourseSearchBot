# uWindsorCourseSearchBot
A Discord bot written in [Rust](https://www.rust-lang.org/) that can query uWindsor course information.

Course information is scraped from [Course Search](https://my.uwindsor.ca/course-search) and a search index is created using [tantivy](https://github.com/tantivy-search/tantivy) based on course code, title, and description.
