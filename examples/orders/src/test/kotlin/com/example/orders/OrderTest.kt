package com.example.orders

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlinx.coroutines.runBlocking

class OrderTest {
    private val catalog = Catalog(mapOf("a" to 100L, "b" to 250L))

    @Test
    fun parsesSkuQuantityPairs() {
        assertEquals(
            Order(listOf(LineItem("a", 2), LineItem("b", 1))),
            Order.parse(listOf("a:2", " b : 1 ")),
        )
    }

    @Test
    fun rejectsWhatIsNotAnOrderLine() {
        assertFailsWith<IllegalArgumentException> { Order.parse(listOf("a")) }
        assertFailsWith<IllegalArgumentException> { Order.parse(listOf("a:x")) }
        assertFailsWith<IllegalArgumentException> { Order.parse(listOf("a:0")) }
    }

    // `subtotalCents` is internal to the main module: the test unit sees it
    // because jrs compiles it with -Xfriend-paths pointing at target/classes.
    @Test
    fun subtotalsInCents() {
        assertEquals(450L, Order.parse(listOf("a:2", "b:1")).subtotalCents(catalog))
    }

    @Test
    fun quotesEveryLine() = runBlocking {
        assertEquals(listOf(200L, 250L), catalog.quote(Order.parse(listOf("a:2", "b:1"))))
    }
}
