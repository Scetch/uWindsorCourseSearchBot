// use chrono::{ Local, TimeZone };
use failure::Error;
use itertools::Itertools;
use reqwest::Client;
use select::{
    document::Document,
    predicate::{ Predicate, Attr, Name, Text, Class },
};
use typemap::Key;

/// Endpoint URL for the course search functionality.
pub static SEARCH_URL: &str = "https://my.uwindsor.ca/web/uw/course-search";
/// URL for directory services.
pub static DIRECTORY_SERVICES: &str = "http://apps.uwindsor.ca/uwincpb/jsp/DirectoryServicesProfile.jsp?q=";

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

#[derive(Debug)]
pub struct Section {
    pub url: String,
    pub code: String,
    pub title: String,
    pub meets: String,
    pub session: String,
    pub status: String,
}

#[derive(Debug)]
pub struct Instructor {
    pub name: String,
    pub title: String,
    pub department: String,
    pub phone: String,
    pub email: String,
}

#[derive(Debug)]
pub struct Exam {
    pub ty: String,
    pub slot: String,
    pub date: String,
    pub time: String,
    pub building: Option<String>,
    pub room: Option<String>,
    pub area: Option<String>,
}

#[derive(Default)]
pub struct Course {
    pub title: String,
    pub meets_desc: String,
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
    pub sections: Vec<Section>,
    pub meets: String,
    // pub last_updated: i64,
}

pub struct Scraper {
    client: Client,
}

impl Key for Scraper {
    type Value = Self;
}

impl Scraper {
    pub fn new() -> Self {
        Scraper {
            client: Client::new(),
        }
    }

    pub fn scrape_course(&self, term: &str, course: &str) -> Result<Option<Course>, Error> {
        let mut parts = course.rsplitn(2, '-');

        let (section, code) = match (parts.next(), parts.next()) {
            (Some(section), Some(course)) => (section, course.split('-').collect::<String>()),
            _ => return Ok(None),
        };

        // Details used by multiple queries
        let details_query = [
            ("_uwinregistrationcoursesearch_WAR_uwinregistrationtoolsportlet_courseDetailsForm.acadtermCode", term),
            ("_uwinregistrationcoursesearch_WAR_uwinregistrationtoolsportlet_courseDetailsForm.activityCode", &code),
            ("_uwinregistrationcoursesearch_WAR_uwinregistrationtoolsportlet_courseDetailsForm.sectionNo", section),
        ];

        //
        // Main Query
        //
        let main_query = [
            ("_uwinregistrationcoursesearch_WAR_uwinregistrationtoolsportlet_struts.portlet.action", "/courseSearch/viewCourseDetails"),
        ];

        let main_resp = self.client.get(SEARCH_URL)
            .query(BASE_QUERY)
            .query(&details_query)
            .query(&main_query)
            .send()
            .and_then(|mut r| r.text())?;

        //println!("{}", main_resp);

        let main_doc = Document::from(main_resp.as_ref());

        // If there was an error on the server end OR this course does not exist
        // we can just return that it doesn't exist.
        if main_doc.find(Class("portlet-msg-error")).next().is_some() {
            return Ok(None);
        }

        let title = main_doc.find(Name("h1").descendant(Text))
            .next()
            .and_then(|n| n.as_text())
            .map(str::trim)
            .map(str::to_owned)
            .ok_or(ParseError("title"))?;

        let details = main_doc.find(Attr("id", "_uwinregistrationcoursesearch_WAR_uwinregistrationtoolsportlet_tabs-details"))
            .next()
            .ok_or(ParseError("details"))?;

        let meets_desc = details.find(Name("div"))
            .next()
            .map(|node| {
                // Meets can sometimes contain extra whitespace in the middle
                // of text. So we'll break up the whitespace and rejoin it.
                node.find(Text)
                    .flat_map(|n| n.as_text())
                    .flat_map(|s| s.split_whitespace())
                    .join(" ")
            })
            .ok_or(ParseError("meets"))?;

        let f = |id: &str| {
            details.find(Attr("id", id).descendant(Text))
                .next()
                .and_then(|n| n.as_text())
                .map(str::trim)
                .map(str::to_owned)
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

        let note = details.find(Class("uwinNoteText"))
            .next()
            .map(|node| {
                node.find(Text)
                    .flat_map(|node| node.as_text())
                    .flat_map(str::split_whitespace)
                    .join(" ")
            });

        let description = details.find(Name("p").descendant(Text))
            .flat_map(|node| node.as_text())
            .flat_map(str::split_whitespace)
            .join(" ");

        let prereqs = main_doc.find({
                Attr("id", "_uwinregistrationcoursesearch_WAR_uwinregistrationtoolsportlet_tabs-prerequistes")
                    .descendant(Name("li"))
            })
            .map(|node| {
                node.find(Text)
                    .flat_map(|node| node.as_text())
                    .flat_map(str::split_whitespace)
                    .join(" ")
            })
            .collect::<Vec<_>>();

        let exams = main_doc.find({
                Attr("id", "_uwinregistrationcoursesearch_WAR_uwinregistrationtoolsportlet_tabs-exams")
                    .descendant(Name("tr"))
            })
            .skip(1) // Skip the title.
            .map(|node| {
                let mut columns = node.find(Name("td").descendant(Text))
                    .flat_map(|n| n.as_text())
                    .map(str::trim)
                    .map(str::to_owned);

                Some(Exam {
                    ty: columns.next()?,
                    slot: columns.next()?,
                    date: columns.next()?,
                    time: columns.next()?,
                    building: columns.next(),
                    room: columns.next(),
                    area: columns.next(),
                })
            })
            .collect::<Option<Vec<_>>>()
            .ok_or(ParseError("exams"))?;

        //
        // Instructor Query
        //
        let inst_query = [
            ("_uwinregistrationcoursesearch_WAR_uwinregistrationtoolsportlet_struts.portlet.action", "/courseSearch/viewCourseDetailsInstructors"),
        ];

        let inst_resp = self.client.get(SEARCH_URL)
            .query(BASE_QUERY)
            .query(&details_query)
            .query(&inst_query)
            .send()
            .and_then(|mut r| r.text())?;

        let inst_doc = Document::from(inst_resp.as_ref());

        let instructors = inst_doc.find(Name("ul").descendant(Name("li")))
            .map(|node| {
                let name = node.find(Name("b").descendant(Text))
                    .next()
                    .and_then(|s| s.as_text())
                    .map(str::trim)
                    .map(str::to_owned)?;

                let mut info = node.find({
                        Name("div")
                            .descendant(Class("wwctrl"))
                            .descendant(Text)
                    })
                    .flat_map(|s| s.as_text())
                    .map(str::trim)
                    .map(str::to_owned);

                Some(Instructor {
                    name: name,
                    title: info.next()?,
                    department: info.next()?,
                    phone: info.next()?,
                    email: info.next()?,
                })
            })
            .collect::<Option<Vec<_>>>()
            .ok_or(ParseError("instructors"))?;

        //
        // Sections Query
        //
        let sections_query = [
            ("_uwinregistrationcoursesearch_WAR_uwinregistrationtoolsportlet_struts.portlet.action", "/courseSearch/executeFindOtherSections"),
            ("_uwinregistrationcoursesearch_WAR_uwinregistrationtoolsportlet_acadtermCode", term),
            ("_uwinregistrationcoursesearch_WAR_uwinregistrationtoolsportlet_courseSearchForm.courseNumber", &code),
        ];

        let resp = self.client.get(SEARCH_URL)
            .query(BASE_QUERY)
            .query(&sections_query)
            .send()
            .and_then(|mut r| r.text())?;

        let sections = Document::from(resp.as_str())
            .find({
                Attr("id", "_uwinregistrationcoursesearch_WAR_uwinregistrationtoolsportlet_OtherSections")
                    .child(Name("table"))
                    .child(Name("tbody"))
                    .child(Name("tr"))
            })
            .map(|node| {
                let mut columns = node.find(Name("td"));

                let a = columns.next()?
                    .find(Class("uwinPopupLink"))
                    .next()?;

                let url = a.attr("href")?
                    .to_owned();

                let code = a.find(Text)
                    .next()?
                    .as_text()?
                    .trim()
                    .to_owned();

                let mut columns = columns.map(|node| {
                        node.find(Text)
                            .flat_map(|n| n.as_text())
                            .flat_map(str::split_whitespace)
                            .join(" ")
                    });

                Some(Section {
                    url: url,
                    code: code,
                    title: columns.next()?,
                    meets: columns.next()?,
                    session: columns.next()?,
                    status: columns.next()?,
                })
            })
            .collect::<Option<Vec<_>>>()
            .ok_or(ParseError("sections"))?;

        let meets = sections.iter()
            .find(|s| s.code == course)
            .map(|s| s.meets.clone())
            .ok_or(ParseError("time"))?;

        Ok(Some(Course {
            title: title,
            meets_desc: meets_desc,
            starts: starts,
            ends: ends,
            campus: campus,
            availability: availability,
            course_value: course_value,
            date_drops_close: date_drops_close,
            description: description,
            prereqs: prereqs,
            note: note,
            exams: exams,
            instructors: instructors,
            sections: sections,
            meets: meets,
            // last_updated: Local::now().timestamp(),
        }))
    }

    /*
    /// Find courses for course `code`
    /// Courses may have multiple sections, this method will only return the first section
    /// that is returned for each course which should be the main course.
    pub fn find_courses(&self, code: &str) -> Result<Vec<Course>, reqwest::Error> {
        let query = [
            ("p_p_id", "uwinregistrationcoursesearch_WAR_uwinregistrationtoolsportlet"),
            ("p_p_lifecycle", "1"),
            ("p_p_state", "normal"),
            ("p_p_mode", "view"),
            ("p_p_col_id", "column-1"),
            ("p_p_col_count", "1"),
            ("_uwinregistrationcoursesearch_WAR_uwinregistrationtoolsportlet_templateDir", "template"),
            ("_uwinregistrationcoursesearch_WAR_uwinregistrationtoolsportlet_theme", "css_xhtml"),
            ("_uwinregistrationcoursesearch_WAR_uwinregistrationtoolsportlet_dynamicAttributes", "{}"),
            ("_uwinregistrationcoursesearch_WAR_uwinregistrationtoolsportlet_struts.portlet.action", "/courseSearch/ExecuteCourseSearch"),
            ("_uwinregistrationcoursesearch_WAR_uwinregistrationtoolsportlet_struts.portlet.mode", "view"),
        ];

        let params = [
            ("acadtermCode", "20185"),
            ("advancedSearch", "false"),
            ("courseSearchForm.acadLevel", ""),
            ("courseSearchForm.courseNumber", code),
            ("courseSearchForm.searchBy", "Course"),
            ("courseSearchForm.subject", " "),
        ];

        let resp = self.0.post(SEARCH_URL)
            .query(&query)
            .form(&params)
            .send()
            .and_then(|mut r| r.text())?;

        let courses = Document::from(resp.as_str())
            .find({
                Attr("id", "_uwinregistrationcoursesearch_WAR_uwinregistrationtoolsportlet_CourseResults")
                    .descendant(Name("tbody"))
                    .descendant(Name("tr"))
            })
            .flat_map(|node| {
                let mut columns = node.find(Name("td"));
                let a = columns.next()?.first_child()?;
                let url = a.attr("href")?;
                let code = a.first_child()?
                    .as_text()?
                    .rsplitn(2, '-')
                    .nth(1)?;
                Some((columns, url, code))
            })
            .group_by(|(_, _, code)| *code)
            .into_iter()
            .flat_map(|(_, mut sections)| {
                let (mut columns, url, code) = sections.next()?;
                Some(Course {
                    url: url.to_owned(),
                    code: code.to_owned(),
                    title: columns.next()?.text(),
                    sections: 1 + sections.count(),
                })
            })
            .collect::<Vec<_>>();

        Ok(courses)
    }
    */
}
