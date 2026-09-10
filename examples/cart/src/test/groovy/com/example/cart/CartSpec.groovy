package com.example.cart

import spock.lang.Specification

class CartSpec extends Specification {
    def "an empty cart costs nothing"() {
        expect:
        new Cart().totalCents() == 0
    }

    def "the subtotal adds up every line"() {
        given:
        def cart = new Cart().add("a", 100, 3).add("b", 250, 2)

        expect:
        cart.subtotalCents() == 800
    }

    def "the best discount wins, and discounts do not stack"() {
        given:
        def cart = new Cart()
                .add("a", 1000, 3)
                .offer(Discount.percent(10))
                .offer(Discount.everyNthFree(3))

        expect:
        cart.discountCents() == 1000
        cart.totalCents() == 2000
    }

    def "a line needs a positive quantity, not #quantity"() {
        when:
        new Cart().add("a", 100, quantity)

        then:
        thrown(IllegalArgumentException)

        where:
        quantity << [0, -1]
    }
}
