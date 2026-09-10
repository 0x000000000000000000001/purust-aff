module Test.Stress where

import Prelude

import Effect.Aff (Aff, forkAff, joinFiber)
import Effect.AVar as AVar
import Effect.Ref as Ref
import Effect.Class (liftEffect)
import Data.Array as Array
import Data.Traversable (traverse, traverse_)
import Test.Assert (assert')

assertAff :: String -> Boolean -> Aff Unit
assertAff s r = liftEffect $ assert' ("Assertion failure " <> s) r

stressAVar :: Aff Unit
stressAVar = do
  avar <- liftEffect AVar.empty
  ref <- liftEffect $ Ref.new 0
  
  let
    producer = liftEffect $ AVar.put unit avar (const (pure unit))
    consumer = do
      _ <- liftEffect $ AVar.take avar (\_ -> Ref.modify_ (_ + 1) ref)
      pure unit
      
  consumers <- traverse forkAff (Array.replicate 1000 consumer)
  producers <- traverse forkAff (Array.replicate 1000 producer)
  
  traverse_ joinFiber consumers
  traverse_ joinFiber producers
  
  -- Wait a bit to ensure all callbacks are processed
  
  val <- liftEffect $ Ref.read ref
  assertAff "stress AVar resolved 1000 items" (val == 1000)

