# Simplicity

## 1. Size

The diff should be smaller 90% of the time. Less code is ALWAYS better.

## 2. Complexity

Reduced cyclomatic complexity is better. The more complex a change is, the less likely it is to be correct. Push back on complexity.

## 3. Abstractions

Single-use abstractions must be eliminated. Just do things inline.

## 4. Readability

An unfamiliar developer should be able to immediately understand the code with zero context, due to a lack of jargon and straightforward STE100-style naming. Code should be readable and simple.

Every API the diff adds or changes, at the site of use, when transformed into a plain English sentence, should make sense. Balance this prose constraint against conciseness and approachable technical language. Can every single item in the API be passed to an agent with zero context, and have it accurately guess the use case?

## 5. Comments

Reject any AI slop comments.

## 6. API use case overlap

Eliminate overlap in use cases. Do two APIs do similar things? If so, why are they separate? How does the user know which one to use for which use case? Should they be merged?

## 7. API size

Rank every single item in the API the diff adds or changes by expected usage:

- 1: Every user, every use case.
- 2: Most users, most use cases.
- 3: Some users, some use cases.
- 4: Rare.

Eliminate 3s and 4s. Add them back only as needed. The API surface must be as small as possible.

## 8. Type overlap

The state represented by types must never overlap. If two states are both partially represented by two types, eliminate the overlap. Inaccurate models of state create garbage code.
