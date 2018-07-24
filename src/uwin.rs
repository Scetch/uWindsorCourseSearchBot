use std::fs;
use std::path::Path;

use failure::Error;
use itertools::Itertools;
use rayon::prelude::*;
use reqwest::Client;
use select::{
    document::Document,
    predicate::{ Predicate, Attr, Name, Text, Class, And },
};
use tantivy::{
    self,
    Index,
    schema::*,
    collector::TopCollector,
    query::*,
    tokenizer::*,
};
use typemap::Key;

/// Endpoint URL for the course search functionality.
static SEARCH_URL: &str = "https://my.uwindsor.ca/web/uw/course-search";
/// URL for directory services.
static DIRECTORY_SERVICES: &str = "http://apps.uwindsor.ca/uwincpb/jsp/DirectoryServicesProfile.jsp?q=";

/// Base query used for every request.
static BASE_QUERY: &[(&str, &str)] = &[
    ("p_p_id", "uwinregistrationcoursesearch_WAR_uwinregistrationtoolsportlet"),
    ("p_p_lifecycle", "0"),
    ("p_p_state", "exclusive"),
    ("p_p_mode", "view"),
    ("p_p_col_id", "column-1"),
    ("p_p_col_count", "1"),
    ("_uwinregistrationcoursesearch_WAR_uwinregistrationtoolsportlet_struts.portlet.mode", "view"),
];

#[derive(Debug, Fail)]
#[fail(display = "Error parsing HTML at {}", _0)]
pub struct ParseError(&'static str);

#[derive(Debug, Fail)]
#[fail(display = "Query is invalid: {:?}", _0)]
pub struct QueryError(QueryParserError);

/// Instructor information
pub struct Instructor {
    pub name: String,
    pub title: Option<String>,
    pub department: Option<String>,
    pub phone: Option<String>,
    pub email: Option<String>,
}

impl Instructor {
    pub fn directory_url(&self) -> Option<String> {
        self.email.as_ref()
            .and_then(|e| e.split('@').next())
            .map(|id| format!("{}{}", DIRECTORY_SERVICES, id))
    }
}

/// Exam information
pub struct Exam {
    pub ty: String,
    pub slot: Option<String>,
    pub date: Option<String>,
    pub time: Option<String>,
    pub building: Option<String>,
    pub room: Option<String>,
    pub area: Option<String>,
}

/// Full course information
pub struct Course {
    pub code: String,
    pub title: String,
    pub meets: String,
    pub starts: String,
    pub ends: String,
    pub campus: String,
    pub availability: String,
    pub course_value: String,
    pub date_drops_close: String,
    pub description: String,
    pub note: Option<String>,
    pub prereqs: Vec<String>,
    pub exams: Vec<Exam>,
    pub instructors: Vec<Instructor>,
}

/// Course preview information that is stored in the index.
/// We save this information when we index all of the courses so
/// we only have to fully scrape a course when we need to.
pub struct CoursePreview<'a> {
    scraper: &'a Scraper,
    pub term: String,
    pub code: String,
    pub title: String,
}

impl<'a> CoursePreview<'a> {
    /// Scrape all information for a course.
    pub fn scrape(&self) -> Result<Course, Error> {
        self.scraper.scrape_full(&self.term, &self.code)
    }
}

/// A search index for all current courses.
pub struct CourseIndex {
    scraper: Scraper,
    index: Index,
    term: Field,
    code: Field,
    title: Field,
    description: Field,
}

impl Key for CourseIndex {
    type Value = Self;
}

impl CourseIndex {
    /// Opens or attempts to create a new index by scraping information from the
    /// university search system.
    pub fn open() -> Result<Self, Error> {
        let ngram = TextOptions::default()
            .set_indexing_options(
                TextFieldIndexing::default()
                    .set_tokenizer("ngram")
                    .set_index_option(IndexRecordOption::WithFreqsAndPositions)
            )
            .set_stored();

        let mut schema_builder = SchemaBuilder::default();
        let term = schema_builder.add_text_field("term", STRING | STORED);
        let code = schema_builder.add_text_field("code", ngram.clone());
        let title = schema_builder.add_text_field("title", ngram);
        let description = schema_builder.add_text_field("description", TEXT);
        let schema = schema_builder.build();

        let path = Path::new("./index");

        let exists = path.is_dir();

        let index = if exists {
            Index::open_in_dir(path)?
        } else {
            fs::create_dir(path)?;
            Index::create_in_dir(path, schema)?
        };

        index.tokenizers()
            .register("ngram", {
                NgramTokenizer::new(3, 3, false)
                    .filter(RemoveLongFilter::limit(40))
                    .filter(LowerCaser)
            });

        let scraper = Scraper::new();

        if !exists {
            let mut index_writer = index.writer(100_000_000)?;

            info!("Scraping course information...");

            let data = scraper.scrape()?;

            info!("Adding course information to index...");

            for (ter, courses) in data {
                for (c, t, d) in courses {
                    let mut doc = tantivy::Document::default();
                    doc.add_text(term, &ter);
                    doc.add_text(code, &c);
                    doc.add_text(title, &t);
                    doc.add_text(description, &d);
                    index_writer.add_document(doc);
                }
            }

            index_writer.commit()?;
            index.load_searchers()?;
        }

        Ok(CourseIndex {
            scraper: scraper,
            index: index,
            term: term,
            code: code,
            title: title,
            description: description,
        })
    }

    /// Returns a list of courses found in the index.
    pub fn query<'a>(&'a self, term: &str, query: &str) -> Result<Vec<CoursePreview<'a>>, Error> {
        // The query string the user has entered.
        let default_fields = vec![self.code, self.title, self.description];
        let user_query = QueryParser::for_index(&self.index, default_fields)
            .parse_query(query)
            .map_err(QueryError)?;

        // The query for the current term (semester).
        let term_query = TermQuery::new(
            Term::from_field_text(self.term, term),
            IndexRecordOption::Basic,
        );

        let query = BooleanQuery::from(vec![
            (Occur::Must, user_query),
            (Occur::Must, Box::new(term_query))
        ]);

        let mut top = TopCollector::with_limit(10);
        let searcher = self.index.searcher();
        searcher.search(&query, &mut top)?;

        top.docs()
            .iter()
            .map(|doc| {
                let doc = searcher.doc(doc)?;
                let term = doc.get_first(self.term).unwrap();
                let code = doc.get_first(self.code).unwrap();
                let title = doc.get_first(self.title).unwrap();

                Ok(CoursePreview {
                    scraper: &self.scraper,
                    term: term.text().to_owned(),
                    code: code.text().to_owned(),
                    title: title.text().to_owned(),
                })
            })
            .collect::<Result<Vec<_>, Error>>()
    }
}

pub struct Scraper(Client);

impl Key for Scraper {
    type Value = Self;
}

impl Scraper {
    fn new() -> Self {
        Scraper(Client::new())
    }

    /// Scrape all terms
    fn scrape(&self) -> Result<Vec<(String, Vec<(String, String, String)>)>, Error> {
        let resp = self.0.get(SEARCH_URL)
            .query(BASE_QUERY)
            .send()
            .and_then(|mut r| r.text())?;

        let doc = Document::from(resp.as_ref());

        doc.find({
                And(Name("select"), Attr("id", "ExecuteCourseSearch_acadtermCode"))
            })
            .next()
            .ok_or(ParseError("term code list"))?
            .children()
            .filter(|node| node.is(Name("option")))
            .map(|node| {
                let code = node.attr("value")
                    .ok_or(ParseError("term code value"))?;

                let courses = self.scrape_courses(code)?;

                let name = node.find(Text)
                    .flat_map(|node| node.as_text())
                    .flat_map(str::split_whitespace)
                    .join(" ");

                if name.is_empty() {
                    return Err(ParseError("term code name").into());
                }

                Ok((code.to_owned(), courses))
            })
            .collect::<Result<Vec<_>, Error>>()
    }

    /// Scrape all courses for a term
    fn scrape_courses(&self, term: &str) -> Result<Vec<(String, String, String)>, Error> {
        let query = [
            ("_uwinregistrationcoursesearch_WAR_uwinregistrationtoolsportlet_struts.portlet.action", "/courseSearch/ExecuteCourseSearch"),
        ];

        let form = [
            ("acadtermCode", term),
            ("advancedSearch", "false"),
            ("courseSearchForm.acadLevel", ""),
            ("courseSearchForm.courseNumber", ""),
            ("courseSearchForm.searchBy", "Course"),
            ("courseSearchForm.subject", " "),
        ];

        let resp = self.0.post(SEARCH_URL)
            .query(BASE_QUERY)
            .query(&query)
            .form(&form)
            .send()
            .and_then(|mut r| r.text())?;

        let doc = Document::from(resp.as_ref());

        doc.find({
                Attr("id", "_uwinregistrationcoursesearch_WAR_uwinregistrationtoolsportlet_CourseResults")
                    .child(Name("table"))
                    .child(Name("tbody"))
            })
            .next()
            .ok_or(ParseError("course list"))?
            .children()
            .filter(|node| node.is(Name("tr")))
            .map(|node| {
                // We only want the list of course codes.
                let code = node.children()
                    .filter(|node| node.is(Name("td")))
                    .next()
                    .map(|node| {
                        node.find(Text)
                            .flat_map(|node| node.as_text())
                            .flat_map(|s| {
                                s.split_whitespace()
                                    .flat_map(|s| s.split("-"))
                            })
                            .collect::<String>()
                    })
                    .filter(|code| !code.is_empty())
                    .ok_or(ParseError("course code"))?;

                Ok(code)
            })
            .collect::<Result<Vec<_>, Error>>()?
            .into_par_iter() // We will get the courses in parallel.
            .map(|code| {
                self.scrape_basic(term, &code)
                    .map(|(title, description)| (code, title, description))
            })
            .collect::<Result<Vec<_>, Error>>()
    }

    /// Scrape the title and description for a given course code for a given term.
    /// This information is used to build the intial search index.
    fn scrape_basic(&self, term: &str, full_code: &str) -> Result<(String, String), Error> {
        let (code, section) = full_code.split_at(7);

        let details_query = [
            ("_uwinregistrationcoursesearch_WAR_uwinregistrationtoolsportlet_courseDetailsForm.acadtermCode", term),
            ("_uwinregistrationcoursesearch_WAR_uwinregistrationtoolsportlet_courseDetailsForm.activityCode", code),
            ("_uwinregistrationcoursesearch_WAR_uwinregistrationtoolsportlet_courseDetailsForm.sectionNo", section),
        ];

        let resp = self.0.get(SEARCH_URL)
            .query(BASE_QUERY)
            .query(&details_query)
            .query(&[
                ("_uwinregistrationcoursesearch_WAR_uwinregistrationtoolsportlet_struts.portlet.action", "/courseSearch/viewCourseDetails"),
            ])
            .send()
            .and_then(|mut r| r.text())?;

        let doc = Document::from(resp.as_ref());

        let title = doc.find({
                Name("body")
                    .child(Name("h1"))
            })
            .next()
            .ok_or(ParseError("course title"))?
            .find(Text)
            .flat_map(|node| node.as_text())
            .flat_map(str::split_whitespace)
            .join(" ");

        let description = doc.find({
                Attr("id", "_uwinregistrationcoursesearch_WAR_uwinregistrationtoolsportlet_tabs-details")
            })
            .next()
            .ok_or(ParseError("course details"))?
            .find({
                Name("p")
                    .descendant(Text)
            })
            .flat_map(|node| node.as_text())
            .flat_map(str::split_whitespace)
            .join(" ");

        Ok((title, description))
    }

    /// Scrape full course information for a given course when requested.
    fn scrape_full(&self, term: &str, full_code: &str) -> Result<Course, Error> {
        let (code, section) = full_code.split_at(7);

        let details_query = [
            ("_uwinregistrationcoursesearch_WAR_uwinregistrationtoolsportlet_courseDetailsForm.acadtermCode", term),
            ("_uwinregistrationcoursesearch_WAR_uwinregistrationtoolsportlet_courseDetailsForm.activityCode", code),
            ("_uwinregistrationcoursesearch_WAR_uwinregistrationtoolsportlet_courseDetailsForm.sectionNo", section),
        ];

        //
        // Main Query
        //
        let resp = self.0.get(SEARCH_URL)
            .query(BASE_QUERY)
            .query(&details_query)
            .query(&[
               ("_uwinregistrationcoursesearch_WAR_uwinregistrationtoolsportlet_struts.portlet.action", "/courseSearch/viewCourseDetails"),
            ])
            .send()
            .and_then(|mut r| r.text())?;

        let doc = Document::from(resp.as_ref());

        let title = doc.find({
                Name("body")
                    .child(Name("h1"))
            })
            .next()
            .ok_or(ParseError("course title"))?
            .find(Text)
            .flat_map(|node| node.as_text())
            .flat_map(str::split_whitespace)
            .join(" ");

        let details = doc.find({
                Attr("id", "_uwinregistrationcoursesearch_WAR_uwinregistrationtoolsportlet_tabs-details")
            })
            .next()
            .ok_or(ParseError("course details"))?;

        let meets = details.children()
            .filter(|node| node.is(Name("div")))
            .next()
            .map(|node| {
                node.find(Text)
                    .flat_map(|n| n.as_text())
                    .flat_map(str::split_whitespace)
                    .join(" ")
            })
            .ok_or(ParseError("meets"))?;

        let f = |id: &str| {
            details.find({
                    Name("div")
                        .descendant(Attr("id", id))
                })
                .next()
                .map(|node| {
                    node.find(Text)
                        .flat_map(|n| n.as_text())
                        .flat_map(str::split_whitespace)
                        .join(" ")
                })
        };

        let starts = f("dateSessionStartsFormatted")
            .ok_or(ParseError("starts"))?;

        let ends = f("dateSessionEndsFormatted")
            .ok_or(ParseError("ends"))?;

        let campus = f("courseSectionInfo_campus")
            .ok_or(ParseError("campus"))?;

        let availability = f("courseSectionInfo_sectionAvailability")
            .ok_or(ParseError("availability"))?;

        let course_value = f("courseSectionInfo_courseValue")
            .ok_or(ParseError("course_value"))?;

        let date_drops_close = f("dateDropsCloseFormatted")
            .ok_or(ParseError("date_drops_close"))?;

        let note = details.find({
                And(Name("p"), Class("uwinNoteText"))
            })
            .next()
            .map(|node| {
                node.find(Text)
                    .flat_map(|node| node.as_text())
                    .flat_map(str::split_whitespace)
                    .join(" ")
            });

        let description = doc.find({
                Attr("id", "_uwinregistrationcoursesearch_WAR_uwinregistrationtoolsportlet_tabs-details")
            })
            .next()
            .ok_or(ParseError("course details"))?
            .find({
                Name("p")
                    .descendant(Text)
            })
            .flat_map(|node| node.as_text())
            .flat_map(str::split_whitespace)
            .join(" ");

        let prereqs = doc.find({
                Attr("id", "_uwinregistrationcoursesearch_WAR_uwinregistrationtoolsportlet_tabs-prerequistes")
                    .child(Name("ul"))
                    .child(Name("li"))
            })
            .map(|node| {
                node.find(Text)
                    .flat_map(|node| node.as_text())
                    .flat_map(str::split_whitespace)
                    .join(" ")
            })
            .collect::<Vec<_>>();

        let exams = doc.find({
                Attr("id", "_uwinregistrationcoursesearch_WAR_uwinregistrationtoolsportlet_tabs-exams")
                    .descendant(Name("tr"))
            })
            .skip(1) // Skip title
            .map(|node| {
                let mut columns = node.find(Name("td").descendant(Text))
                    .flat_map(|n| n.as_text())
                    .map(str::trim)
                    .map(str::to_owned);

                Some(Exam {
                    ty: columns.next()?,
                    slot: columns.next(),
                    date: columns.next(),
                    time: columns.next(),
                    building: columns.next(),
                    room: columns.next(),
                    area: columns.next(),
                })
            })
            .collect::<Option<Vec<_>>>()
            .ok_or(ParseError("course exam"))?;

        //
        // Instructor Query
        //
        let resp = self.0.get(SEARCH_URL)
            .query(BASE_QUERY)
            .query(&details_query)
            .query(&[
                ("_uwinregistrationcoursesearch_WAR_uwinregistrationtoolsportlet_struts.portlet.action", "/courseSearch/viewCourseDetailsInstructors"),
            ])
            .send()
            .and_then(|mut r| r.text())?;

        let doc = Document::from(resp.as_ref());

        let instructors = doc.find({
                And(Name("ul"), Class("uwinListView"))
            })
            .next()
            .ok_or(ParseError("course instructors"))?;

        let instructors = instructors.children()
            .filter(|node| node.is(Name("li")))
            .map(|node| {
                let name = node.find({
                        Name("b")
                    })
                    .next()
                    .map(|node| {
                        node.find(Text)
                            .flat_map(|node| node.as_text())
                            .flat_map(str::split_whitespace)
                            .join(" ")
                    })
                    .ok_or(ParseError("instructor name"))?;

                let mut info = node.find({
                        Name("div")
                            .child(Class("wwctrl"))
                    })
                    .map(|node| {
                        node.find(Text)
                            .flat_map(|s| s.as_text())
                            .flat_map(str::split_whitespace)
                            .join(" ")
                    });

                Ok(Instructor {
                    name: name,
                    title: info.next(),
                    department: info.next(),
                    phone: info.next(),
                    email: info.next(),
                })
            })
            .collect::<Result<Vec<_>, Error>>()?;

        Ok(Course {
            code: full_code.to_string(),
            title: title,
            meets: meets,
            starts: starts,
            ends: ends,
            campus: campus,
            availability: availability,
            course_value: course_value,
            date_drops_close: date_drops_close,
            description: description,
            note: note,
            prereqs: prereqs,
            exams: exams,
            instructors: instructors,
        })
    }
}
