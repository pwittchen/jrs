package com.example.bookmarks;

import static org.springframework.test.web.servlet.request.MockMvcRequestBuilders.delete;
import static org.springframework.test.web.servlet.request.MockMvcRequestBuilders.get;
import static org.springframework.test.web.servlet.request.MockMvcRequestBuilders.post;
import static org.springframework.test.web.servlet.result.MockMvcResultMatchers.content;
import static org.springframework.test.web.servlet.result.MockMvcResultMatchers.header;
import static org.springframework.test.web.servlet.result.MockMvcResultMatchers.jsonPath;
import static org.springframework.test.web.servlet.result.MockMvcResultMatchers.status;

import org.junit.jupiter.api.Test;
import org.springframework.beans.factory.annotation.Autowired;
import org.springframework.boot.test.context.SpringBootTest;
import org.springframework.boot.webmvc.test.autoconfigure.AutoConfigureMockMvc;
import org.springframework.http.MediaType;
import org.springframework.test.web.servlet.MockMvc;

/**
 * The whole application, started for real and driven over MockMvc: the context
 * loads, the properties bind, the JSON comes out as the records describe it.
 */
@SpringBootTest
@AutoConfigureMockMvc
class BookmarkApiTest {

    @Autowired private MockMvc mvc;

    @Test
    void the_seeded_bookmarks_are_listed() throws Exception {
        mvc.perform(get("/api/bookmarks"))
                .andExpect(status().isOk())
                .andExpect(content().contentType(MediaType.APPLICATION_JSON))
                .andExpect(jsonPath("$[0].title").value("jrs, a Java build system"))
                .andExpect(jsonPath("$[1].url").value("https://spring.io/projects/spring-boot"));
    }

    @Test
    void a_posted_bookmark_comes_back_from_its_location() throws Exception {
        String location = mvc.perform(post("/api/bookmarks")
                        .contentType(MediaType.APPLICATION_JSON)
                        .content("{\"url\":\"https://example.com\",\"title\":\"Example\"}"))
                .andExpect(status().isCreated())
                .andExpect(header().exists("Location"))
                .andReturn()
                .getResponse()
                .getHeader("Location");

        mvc.perform(get(location))
                .andExpect(status().isOk())
                .andExpect(jsonPath("$.title").value("Example"));

        mvc.perform(delete(location)).andExpect(status().isNoContent());
        mvc.perform(get(location)).andExpect(status().isNotFound());
    }

    @Test
    void an_unknown_id_is_a_404() throws Exception {
        mvc.perform(get("/api/bookmarks/9999")).andExpect(status().isNotFound());
    }

    @Test
    void the_static_page_is_served_from_the_resources() throws Exception {
        mvc.perform(get("/")).andExpect(status().isOk());
    }
}
