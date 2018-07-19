extern crate chrono;
extern crate failure;
#[macro_use] extern crate failure_derive;
extern crate itertools;
extern crate reqwest;
extern crate select;
extern crate serenity;
extern crate typemap;

use std::fmt::Write;

// use chrono::{ Local, TimeZone };
use failure::{ Error, SyncFailure };
use itertools::Itertools;
use serenity::{
    prelude::*,
    model::{ channel::Message, id::ChannelId },
};

use uwin::Scraper;

mod uwin;

static IMAGE_DATA: &[u8] = include_bytes!("../uw_logo.png");

fn main() {
    let token = env!("DISCORD_TOKEN");

    let mut client = Client::new(token, Handler)
        .expect("Error creating discord client.");

    client.data.lock().insert::<Scraper>(Scraper::new());

    if let Err(e) = client.start() {
        println!("Client error: {:?}", e);
    }
}

struct Handler;

impl Handler {
    fn fetch_course<'a, A>(&self, ctx: Context, mut args: A, chan: ChannelId) -> Result<(), Error>
        where A: Iterator<Item = &'a str>
    {
        let code = if let Some(code) = args.next() {
            code
        } else {
            chan.send_message(|m| {
                    m.embed(|e| {
                        e.color(0x00005696)
                         .field("Usage", "~course <code>", false)
                         .field("Example", "~course 03-60-100-01", false)
                    })
                })
                .map_err(SyncFailure::new)?;

            return Ok(())
        };

        let mut data = ctx.data.lock();

        let course = data.get_mut::<Scraper>()
            .unwrap()
            .scrape_course("20185", code)?;

        // Make the bot seem like it's typing a message just in case the query
        // takes longer than expected
        chan.broadcast_typing().map_err(SyncFailure::new)?;

        if let Some(course) = course {
            let description = course.description
                .chars()
                .take(200)
                .chain("...\n\n".chars())
                .collect::<String>();

            let instructors = course.instructors
                .iter()
                .fold(String::new(), |mut s, ins| {
                    if let Some(id) = ins.email.split('@').next() {
                        let _ = writeln!(s, "[{}]({}{})", ins.name, uwin::DIRECTORY_SERVICES, id);
                    } else {
                        let _ = writeln!(s, "{}", ins.name);
                    }

                    s
                });

            let exams = course.exams
                .iter()
                .fold(String::new(), |mut s, ex| {
                    let _ = writeln!(s, "**{}** {} {}", ex.ty, ex.date, ex.time);
                    s
                });

            /*
            let time = Local.timestamp(course.last_updated, 0).format("%v %r");
            let timestamp = format!("Last update on {}", time);
            */

            let files = vec![(IMAGE_DATA, "icon.png")];
            chan.send_files(files, |m| {
                    m.embed(|e| {
                        let e = e.color(0x00005696)
                            .title(&course.title)
                            .description(description);

                        // Note can be optional.
                        let e = if let Some(note) = &course.note {
                            e.field("Note", note, false)
                        } else {
                            e
                        };

                        let e = e.field("Meets", &course.meets, true)
                            .field("Availability", &course.availability, true)
                            .field("Instructors", instructors, false);

                        let e = if course.prereqs.len() > 0 {
                            let prereqs = course.prereqs
                                .iter()
                                .join("\n");

                            e.field("Prerequisites", prereqs, false)
                        } else {
                            e
                        };

                        e.field("Exams", exams, false)
                            .thumbnail("attachment://icon.png")
                            // .footer(|f| f.text(timestamp))
                    })
                })
                .map_err(SyncFailure::new)?;
        } else {
            chan.send_message(|m| m.content(format!("Course `{}` not found.", code)))
                .map_err(SyncFailure::new)?;
        }

        Ok(())
    }
}

impl EventHandler for Handler {
    fn message(&self, ctx: Context, msg: Message) {
        let mut args = msg.content.split(' ');

        let cmd = match args.next() {
            Some("~course") => self.fetch_course(ctx, args, msg.channel_id),
            // Some("~search") => Ok(()),
            _ => Ok(()),
        };

        if let Err(e) = cmd {
            println!("Error: {}", e);

            let _ = msg.channel_id
                .send_message(|m| m.content("Internal error."));
        }
    }
}
